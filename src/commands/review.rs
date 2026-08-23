//! Patchsets and reviews can be recorded separately or composed safely.
//! A composed review snapshots only a clean, checked-out change worktree so
//! the verdict binds to the exact committed head the reviewer inspected.

use super::*;
use crate::state::{FindingState, VerdictEntry};
use crate::status::{FindingSummary, StatusReport};
use serde::Serialize;

#[derive(Serialize)]
struct ReviewView<'a> {
    schema: &'static str,
    change_id: &'a str,
    verdicts: Vec<ReviewVerdict<'a>>,
    open_findings: Vec<&'a FindingSummary>,
    has_valid_approval: bool,
    next_action: &'a str,
}

#[derive(Serialize)]
struct ReviewVerdict<'a> {
    verdict: Verdict,
    causes: &'a [ReviewCause],
    patchset_id: &'a str,
    actor: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_behalf_of: Option<&'a str>,
    created_at: chrono::DateTime<chrono::Utc>,
    valid_for_current_head: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    brief_ref: Option<&'a BriefRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    brief_version: Option<usize>,
    findings: Vec<&'a FindingState>,
}

pub fn read_review(ctx: &Ctx, reference: &str, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let (_, state) = ctx.load_state(&store, reference)?;
    let report = ctx.report(&store, &state)?;
    let view = ReviewView {
        schema: "arc-review/1",
        change_id: &state.change_id,
        verdicts: state
            .verdicts
            .iter()
            .rev()
            .map(|verdict| review_verdict(verdict, &state, &report))
            .collect(),
        open_findings: report
            .findings
            .iter()
            .filter(|finding| finding.status == "open")
            .collect(),
        has_valid_approval: report
            .verdict
            .as_ref()
            .is_some_and(|verdict| verdict.valid_for_current_head),
        next_action: &report.next_action,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }

    println!("# Review: {}", state.change_id);
    println!("\n## Verdict history\n");
    if view.verdicts.is_empty() {
        println!("No verdicts recorded.");
    }
    for verdict in &view.verdicts {
        let reviewer = verdict
            .on_behalf_of
            .map(|subject| format!("{} (for {subject})", verdict.actor))
            .unwrap_or_else(|| verdict.actor.to_string());
        println!(
            "- {:?} on `{}` by {} at {} — {}",
            verdict.verdict,
            verdict.patchset_id,
            reviewer,
            verdict.created_at.to_rfc3339(),
            if verdict.valid_for_current_head {
                "valid for current head"
            } else {
                "STALE for current head"
            }
        );
        if let Some(body) = verdict.body {
            println!("  {body}");
        }
        if let (Some(brief_ref), Some(version)) = (verdict.brief_ref, verdict.brief_version) {
            println!("  - brief: v{version} (`{}`)", brief_ref.event_id);
        }
        for finding in &verdict.findings {
            println!(
                "  - `{}` [{}{:?}] {}",
                finding.id,
                if finding.blocking { "blocking/" } else { "" },
                finding.severity,
                finding.summary
            );
        }
    }

    println!("\n## Open findings\n");
    if view.open_findings.is_empty() {
        println!("No open findings.");
    } else {
        for finding in &view.open_findings {
            println!(
                "- `{}` [{}{:?}] {}",
                finding.id,
                if finding.blocking { "blocking/" } else { "" },
                finding.severity,
                finding.summary
            );
        }
    }
    println!("\n## Review needed\n");
    println!(
        "Valid approval for current head: {}",
        if view.has_valid_approval { "yes" } else { "no" }
    );
    println!("Next action: {}", view.next_action);
    Ok(())
}

fn review_verdict<'a>(
    verdict: &'a VerdictEntry,
    state: &'a ChangeState,
    report: &StatusReport,
) -> ReviewVerdict<'a> {
    let valid_for_current_head = state.latest_verdict().is_some_and(|latest| {
        latest.event_id == verdict.event_id
            && report
                .verdict
                .as_ref()
                .is_some_and(|current| current.valid_for_current_head)
    });
    let patchset = state
        .patchsets
        .iter()
        .find(|patchset| patchset.id == verdict.patchset_id);
    ReviewVerdict {
        verdict: verdict.verdict,
        causes: &verdict.causes,
        patchset_id: &verdict.patchset_id,
        actor: &verdict.actor,
        on_behalf_of: verdict.on_behalf_of.as_deref(),
        created_at: verdict.created_at,
        valid_for_current_head,
        body: verdict.body.as_deref(),
        brief_ref: patchset.and_then(|patchset| patchset.brief_ref.as_ref()),
        brief_version: patchset.and_then(|patchset| patchset.brief_version),
        findings: state
            .findings
            .values()
            .filter(|finding| finding.origin_event == verdict.event_id)
            .collect(),
    }
}

pub fn snapshot(
    ctx: &Ctx,
    reference: &str,
    base: Option<String>,
    brief_version: Option<usize>,
) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let events = store.load_events(&change_id)?;
    let st = state::reduce(&events)?;
    let head = gitio::branch_head(&ctx.cwd, &st.branch)?;
    let merge_base = gitio::branch_head(&ctx.cwd, &st.target_branch)
        .ok()
        .and_then(|target_head| gitio::merge_base(&ctx.cwd, &target_head, &head).ok());
    let base_rev = match base {
        Some(b) => gitio::rev_parse(&ctx.cwd, &b)?,
        None => merge_base.clone().unwrap_or_else(|| st.base.clone()),
    };
    let brief_ref = match brief_version {
        Some(0) => bail!("brief version 0 not found"),
        Some(version) => Some(
            st.briefs
                .get(version - 1)
                .with_context(|| format!("brief version {version} not found"))?,
        ),
        None => st.latest_brief(),
    }
    .map(|brief| BriefRef {
        event_id: brief.event_id.clone(),
    });
    let unchanged_patchset = st
        .latest_patchset()
        .filter(|p| p.head == head && p.base == base_rev && p.brief_ref == brief_ref)
        .map(|p| p.id.clone());
    let identity = gitio::commit_identity(&ctx.cwd, &head)?;
    let now = chrono::Utc::now();
    let snapshot_claim = st
        .claim
        .as_ref()
        .filter(|claim| state::claim_timing_at(claim, now).active);
    let patchset_id = format!("ps-{:02}", st.patchsets.len() + 1);
    let payload = Payload::PatchsetAdded {
        patchset_id: patchset_id.clone(),
        base: base_rev,
        head: head.clone(),
        merge_base,
        brief_ref,
        author_name: Some(identity.author_name),
        author_email: Some(identity.author_email),
        committer_name: Some(identity.committer_name),
        committer_email: Some(identity.committer_email),
        claim_id: snapshot_claim.map(|claim| claim.claim_id.clone()),
        claim_actor: snapshot_claim.map(|claim| claim.owner.actor.clone()),
    };
    ensure_append_allowed(&st, &payload)?;
    if let Some(patchset_id) = unchanged_patchset {
        println!("patchset: {patchset_id} (unchanged)");
        return Ok(());
    }
    let mut ev = ctx.event_at(&store, &change_id, now, payload);
    ev.event_id = event_id_after(
        &events
            .last()
            .context("change has no opening event")?
            .event_id,
    )?;
    store.append_event(&ev)?;
    // Pin this head with its own ref: reviewed heads must stay reachable
    // individually, even if the branch is rewound or deleted later.
    gitio::update_ref(
        &ctx.cwd,
        &gitio::retention_ref(&change_id, &patchset_id),
        &head,
    )?;
    println!("patchset: {patchset_id}");
    println!("head: {head}");
    println!("event: {}", ev.event_id);
    Ok(())
}

pub fn comment(
    ctx: &Ctx,
    reference: &str,
    body: String,
    patchset: Option<String>,
    anchor_args: &AnchorArgs,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let patchset_id = resolve_patchset_id(&st, patchset)?;
    let anchor = build_anchor(ctx, &st, patchset_id.as_deref(), anchor_args)?;
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::CommentAdded {
            body,
            patchset_id,
            anchor,
        },
    );
    store.append_event(&ev)?;
    println!("event: {}", ev.event_id);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn finding(
    ctx: &Ctx,
    reference: &str,
    summary: String,
    body: Option<String>,
    blocking: bool,
    severity: Severity,
    patchset: Option<String>,
    anchor_args: &AnchorArgs,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, _transition, st) = locked_state(&store, reference)?;
    let patchset_id = resolve_patchset_id(&st, patchset)?;
    let anchor = build_anchor(ctx, &st, patchset_id.as_deref(), anchor_args)?;
    let finding_id = ids::new_finding_id();
    let payload = Payload::FindingAdded {
        finding_id: finding_id.clone(),
        blocking,
        severity,
        summary,
        body,
        patchset_id,
        anchor,
    };
    ensure_append_allowed(&st, &payload)?;
    let ev = ctx.event(&store, &change_id, payload);
    store.append_event(&ev)?;
    println!("finding: {finding_id}");
    println!("event: {}", ev.event_id);
    Ok(())
}

pub fn reply(ctx: &Ctx, reference: &str, parent_event_id: String, body: String) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, _) = ctx.load_state(&store, reference)?;
    let parent_event_id = store
        .resolve_discussion_event(&change_id, &parent_event_id)?
        .event_id;
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::ReplyAdded {
            parent_event_id,
            body,
        },
    );
    store.append_event(&ev)?;
    println!("event: {}", ev.event_id);
    Ok(())
}

pub fn resolve(
    ctx: &Ctx,
    reference: &str,
    finding: String,
    disposition: DispositionStatus,
    commit: Option<String>,
    evidence: Option<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, _transition, st) = locked_state(&store, reference)?;
    let (finding_id, audit) = match store.resolve_discussion_event(&change_id, &finding) {
        Ok(event) => match event.payload {
            Payload::FindingAdded { finding_id, .. } => (finding_id, false),
            Payload::AuditFindingAdded { finding_id, .. } => (finding_id, true),
            Payload::CommentAdded { .. } => {
                bail!("discussion event {finding:?} is a comment, not a finding")
            }
            _ => unreachable!("discussion event resolution filters payloads"),
        },
        Err(_error) if st.findings.contains_key(&finding) => {
            (st.resolve_finding_id(&finding)?, false)
        }
        Err(_error) if st.audit_findings.contains_key(&finding) => (
            crate::state::resolve_unique_id(
                st.audit_findings.keys().map(String::as_str),
                &finding,
                "audit finding",
            )?,
            true,
        ),
        Err(error) => return Err(error),
    };
    let commit = match commit {
        Some(c) => Some(gitio::rev_parse(&ctx.cwd, &c)?),
        None => None,
    };
    let selected = if audit {
        &st.audit_findings[&finding_id]
    } else {
        &st.findings[&finding_id]
    };
    let supersedes: Vec<String> = selected.tips().iter().map(|t| t.event_id.clone()).collect();
    let payload = if audit {
        Payload::AuditDispositionRecorded {
            finding_id: finding_id.clone(),
            status: disposition,
            commit,
            evidence,
            supersedes,
        }
    } else {
        Payload::DispositionRecorded {
            finding_id: finding_id.clone(),
            status: disposition,
            commit,
            evidence,
            supersedes,
        }
    };
    ensure_append_allowed(&st, &payload)?;
    let ev = ctx.event(&store, &change_id, payload);
    store.append_event(&ev)?;
    println!("finding: {finding_id} → {disposition:?}");
    println!("event: {}", ev.event_id);
    Ok(())
}

pub struct ReviewArgs {
    pub verdict: Verdict,
    pub body: Option<String>,
    pub provisional: Option<String>,
    pub patchset: Option<String>,
    pub causes: Vec<ReviewCause>,
    pub findings_json: Option<String>,
    pub snapshot_first: bool,
}

pub fn review(ctx: &Ctx, reference: &str, args: ReviewArgs) -> Result<()> {
    let ReviewArgs {
        verdict,
        body,
        provisional,
        patchset,
        mut causes,
        findings_json,
        snapshot_first,
    } = args;
    causes.sort_unstable();
    causes.dedup();
    if provisional.is_some() && verdict != Verdict::Approved {
        // Only an approval discharges the review gate, so only an approval
        // can owe corroboration for having done so. Recording the marker on
        // a verdict that gates nothing would leave it in the ledger with no
        // advisory, no query, and no discharge — tracked and invisible, which
        // is the state this flag exists to end.
        bail!("--provisional is only valid with --verdict approved");
    }
    let provisional = match provisional {
        Some(reason) if reason.trim().is_empty() => bail!(
            "--provisional must say why this verdict is owed corroboration; an empty \
             reason records an obligation nobody can discharge knowingly"
        ),
        other => other.map(|reason| reason.trim().to_string()),
    };
    match verdict {
        Verdict::ChangesRequested if causes.is_empty() => {
            bail!("--cause is required with --verdict changes-requested")
        }
        Verdict::Approved | Verdict::CommentOnly if !causes.is_empty() => {
            bail!("--cause is only valid with --verdict changes-requested")
        }
        _ => {}
    }
    if snapshot_first {
        if patchset.is_some() {
            bail!("--snapshot cannot be combined with --patchset");
        }
        let store = ctx.store()?;
        let (_, st) = ctx.load_state(&store, reference)?;
        if gitio::current_branch(&ctx.cwd)?.as_deref() != Some(st.branch.as_str())
            || !gitio::is_clean(&ctx.cwd)?
        {
            bail!("review --snapshot requires the change branch checked out in a clean worktree");
        }
        snapshot(ctx, reference, None, None)?;
    }
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let events = store.load_events(&change_id)?;
    let st = state::reduce(&events)?;
    let patchset_id = resolve_patchset_id(&st, patchset)?
        .context("no patchset to review; run `arc snapshot` first")?;

    let inline: Vec<InlineFinding> = match findings_json {
        None => Vec::new(),
        Some(src) => read_finding_inputs(&src)?
            .into_iter()
            .map(|f| {
                let anchor = f.anchor.map(|a| {
                    let anchor_args = AnchorArgs {
                        path: Some(a.path),
                        side: a.side,
                        line_start: a.line_start,
                        line_end: a.line_end,
                        context: a.context,
                    };
                    build_anchor(ctx, &st, Some(&patchset_id), &anchor_args)
                        .ok()
                        .flatten()
                });
                InlineFinding {
                    finding_id: ids::new_finding_id(),
                    blocking: f.blocking,
                    severity: f.severity,
                    summary: f.summary,
                    body: f.body,
                    anchor: anchor.flatten(),
                }
            })
            .collect(),
    };

    if verdict == Verdict::Approved && inline.iter().any(|f| f.blocking) {
        bail!("cannot approve while recording blocking findings in the same review");
    }

    let finding_ids: Vec<String> = inline.iter().map(|f| f.finding_id.clone()).collect();
    let payload = Payload::VerdictRecorded {
        patchset_id: patchset_id.clone(),
        verdict,
        causes,
        body,
        findings: inline,
        provisional: provisional.clone(),
    };
    ensure_append_allowed(&st, &payload)?;
    let mut ev = ctx.event(&store, &change_id, payload);
    ev.event_id = event_id_after(
        &events
            .last()
            .context("change has no opening event")?
            .event_id,
    )?;
    store.append_event(&ev)?;
    println!("verdict: {verdict:?} on {patchset_id}");
    if let Some(reason) = &provisional {
        println!("provisional: {reason}");
    }
    for id in finding_ids {
        println!("finding: {id}");
    }
    println!("event: {}", ev.event_id);
    report_inert_approval(ctx, &store, &change_id)?;
    Ok(())
}

/// Say so when the approval just recorded cannot gate.
///
/// Appending it is correct — a verdict is a fact about what someone concluded,
/// not a request for permission. Reporting success for an act with no effect
/// is what teaches an operator the guard is absent, so the write path
/// evaluates the same policy `check` does and names the outcome on the spot.
fn report_inert_approval(ctx: &Ctx, store: &Store, change_id: &str) -> Result<()> {
    let st = state::reduce(&store.load_events(change_id)?)?;
    let report = ctx.report(store, &st)?;
    let Some(reason) = report.approval_rejection_reason.as_deref() else {
        return Ok(());
    };
    println!("note: this approval does not gate — {reason}.");
    println!(
        "      integration will still refuse. Record the review this change owes with \
`arc integrate {change_id} --audit-debt <reason>`, or obtain a verdict from a \
different actor."
    );
    Ok(())
}

fn build_anchor(
    ctx: &Ctx,
    st: &ChangeState,
    patchset_id: Option<&str>,
    args: &AnchorArgs,
) -> Result<Option<Anchor>> {
    let Some(path) = &args.path else {
        if args.line_start.is_some() {
            bail!("--line requires --path");
        }
        return Ok(None);
    };
    let patchset = match patchset_id {
        Some(id) => st.patchsets.iter().find(|p| p.id == id),
        None => st.latest_patchset(),
    };
    let blob = patchset.and_then(|p| {
        let rev = match args.side {
            Side::Base => &p.base,
            Side::Head => &p.head,
        };
        gitio::blob_oid(&ctx.cwd, rev, path)
    });
    Ok(Some(Anchor {
        path: path.clone(),
        side: args.side,
        blob,
        line_start: args.line_start,
        line_end: args.line_end.or(args.line_start),
        context: args.context.clone(),
    }))
}

/// A patchset named by its own id, or by the revision it recorded.
///
/// A reviewer reports on a revision — it read `8c839c1`, not `ps-06`. Making
/// the lead translate that by hand is where a verdict gets attached to the
/// wrong patchset, so accept either. A revision may be abbreviated, and must
/// identify exactly one patchset: two patchsets can share a head when a brief
/// was renegotiated without new commits, and guessing between them would
/// reintroduce the error this exists to prevent.
fn resolve_patchset_id(st: &ChangeState, patchset: Option<String>) -> Result<Option<String>> {
    let Some(reference) = patchset else {
        return Ok(st.latest_patchset().map(|p| p.id.clone()));
    };
    if st.patchsets.iter().any(|p| p.id == reference) {
        return Ok(Some(reference));
    }
    let matches: Vec<&str> = st
        .patchsets
        .iter()
        .filter(|p| p.head.starts_with(&reference))
        .map(|p| p.id.as_str())
        .collect();
    match matches.as_slice() {
        [single] => Ok(Some((*single).to_string())),
        [] => bail!("unknown patchset {reference:?}: no patchset has that id or revision"),
        many => bail!(
            "revision {reference:?} matches {}; name the patchset instead",
            many.join(", ")
        ),
    }
}

/// Read a findings batch from a file or stdin.
fn read_finding_inputs(src: &str) -> Result<Vec<FindingInput>> {
    let text = if src == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        std::fs::read_to_string(src).with_context(|| format!("cannot read findings file {src}"))?
    };
    serde_json::from_str(&text).context("malformed findings JSON")
}

/// The findings batch for an event that has no patchset to anchor against.
/// Audits review an integrated revision, so line anchors — which resolve
/// through a patchset diff — are not offered rather than silently dropped.
pub(crate) fn parse_inline_findings(src: Option<&str>) -> Result<Vec<InlineFinding>> {
    let Some(src) = src else {
        return Ok(Vec::new());
    };
    read_finding_inputs(src)?
        .into_iter()
        .map(|f| {
            if f.anchor.is_some() {
                bail!("audit findings cannot carry a line anchor; anchors resolve through a patchset diff");
            }
            Ok(InlineFinding {
                finding_id: ids::new_finding_id(),
                blocking: f.blocking,
                severity: f.severity,
                summary: f.summary,
                body: f.body,
                anchor: None,
            })
        })
        .collect()
}
