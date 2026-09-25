//! External review records retain their source and never impersonate a local verdict.

use crate::commands::Ctx;
use crate::gitio;
use crate::model::{ExternalFinding, ExternalVerdict, Payload};
use anyhow::{bail, Context, Result};

pub struct ExternalVerdictArgs {
    pub verdict: ExternalVerdict,
    pub decided_by: String,
    pub reference: String,
    pub revision: String,
    pub findings_json: Option<String>,
}

pub fn record_verdict(ctx: &Ctx, reference: &str, args: ExternalVerdictArgs) -> Result<()> {
    let store = ctx.store()?;
    ctx.ensure_declared_actor(&store)?;
    let (change_id, _transition, state) = super::locked_state(&store, reference)?;
    let revision = gitio::rev_parse(&ctx.cwd, &args.revision)?;
    let decided_by = nonempty_line(args.decided_by, "--decided-by")?;
    let source_reference = nonempty_line(args.reference, "--reference")?;
    let findings = args
        .findings_json
        .as_deref()
        .map(super::review::read_finding_inputs)
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .map(|finding| ExternalFinding {
            finding_id: crate::ids::new_finding_id(),
            blocking: finding.blocking,
            severity: finding.severity,
            summary: finding.summary,
            body: finding.body,
            anchor: finding.anchor,
        })
        .collect::<Vec<_>>();

    if args.verdict != ExternalVerdict::ChangesRequested && !findings.is_empty() {
        bail!("external findings may be attached only to a changes-requested verdict");
    }

    let payload = Payload::ExternalVerdictRecorded {
        revision: revision.clone(),
        verdict: args.verdict,
        decided_by: decided_by.clone(),
        reference: source_reference.clone(),
        findings: findings.clone(),
    };
    super::ensure_append_allowed(&state, &payload)?;
    let events = store.load_events(&change_id)?;
    let previous = events
        .last()
        .context("change has no opening event")?
        .event_id
        .as_str();
    let mut event = ctx.event(&store, &change_id, payload);
    event.event_id = super::event_id_after(previous)?;
    store.append_event(&event)?;

    println!(
        "external verdict: {:?} at {} by {} (reference {})",
        args.verdict, revision, decided_by, source_reference
    );
    for finding in findings {
        println!("external finding: {}", finding.finding_id);
    }
    if args.verdict == ExternalVerdict::Rejected
        && state
            .latest_patchset()
            .is_some_and(|patchset| patchset.head == revision)
    {
        println!("closed: {change_id} (abandoned by the external rejection)");
    }
    println!("event: {}", event.event_id);
    Ok(())
}

fn nonempty_line(value: String, flag: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.contains('\n') || value.contains('\r') {
        bail!("{flag} must be one nonempty line");
    }
    Ok(value.to_string())
}
