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
    patchset_id: &'a str,
    actor: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_behalf_of: Option<&'a str>,
    created_at: chrono::DateTime<chrono::Utc>,
    valid_for_current_head: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
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
    ReviewVerdict {
        verdict: verdict.verdict,
        patchset_id: &verdict.patchset_id,
        actor: &verdict.actor,
        on_behalf_of: verdict.on_behalf_of.as_deref(),
        created_at: verdict.created_at,
        valid_for_current_head,
        body: verdict.body.as_deref(),
        findings: state
            .findings
            .values()
            .filter(|finding| finding.origin_event == verdict.event_id)
            .collect(),
    }
}

pub fn snapshot(ctx: &Ctx, reference: &str, base: Option<String>) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let events = store.load_events(&change_id)?;
    let st = state::reduce(&events)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let head = gitio::branch_head(&ctx.cwd, &st.branch)?;
    let merge_base = gitio::branch_head(&ctx.cwd, &st.target_branch)
        .ok()
        .and_then(|target_head| gitio::merge_base(&ctx.cwd, &target_head, &head).ok());
    let base_rev = match base {
        Some(b) => gitio::rev_parse(&ctx.cwd, &b)?,
        None => merge_base.clone().unwrap_or_else(|| st.base.clone()),
    };
    if let Some(p) = st.latest_patchset() {
        if p.head == head && p.base == base_rev {
            println!("patchset: {} (unchanged)", p.id);
            return Ok(());
        }
    }
    let identity = gitio::commit_identity(&ctx.cwd, &head)?;
    let now = chrono::Utc::now();
    let snapshot_claim = st
        .claim
        .as_ref()
        .filter(|claim| state::claim_timing_at(claim, now).active);
    let patchset_id = format!("ps-{:02}", st.patchsets.len() + 1);
    let mut ev = ctx.event_at(
        &store,
        &change_id,
        now,
        Payload::PatchsetAdded {
            patchset_id: patchset_id.clone(),
            base: base_rev,
            head: head.clone(),
            merge_base,
            author_name: Some(identity.author_name),
            author_email: Some(identity.author_email),
            committer_name: Some(identity.committer_name),
            committer_email: Some(identity.committer_email),
            claim_id: snapshot_claim.map(|claim| claim.claim_id.clone()),
            claim_actor: snapshot_claim.map(|claim| claim.owner.actor.clone()),
        },
    );
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
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let patchset_id = resolve_patchset_id(&st, patchset)?;
    let anchor = build_anchor(ctx, &st, patchset_id.as_deref(), anchor_args)?;
    let finding_id = ids::new_finding_id();
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::FindingAdded {
            finding_id: finding_id.clone(),
            blocking,
            severity,
            summary,
            body,
            patchset_id,
            anchor,
        },
    );
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
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let finding_id = match store.resolve_discussion_event(&change_id, &finding) {
        Ok(event) => match event.payload {
            Payload::FindingAdded { finding_id, .. } => finding_id,
            Payload::CommentAdded { .. } => {
                bail!("discussion event {finding:?} is a comment, not a finding")
            }
            _ => unreachable!("discussion event resolution filters payloads"),
        },
        Err(_error) if st.findings.contains_key(&finding) => st.resolve_finding_id(&finding)?,
        Err(error) => return Err(error),
    };
    let commit = match commit {
        Some(c) => Some(gitio::rev_parse(&ctx.cwd, &c)?),
        None => None,
    };
    let supersedes: Vec<String> = st.findings[&finding_id]
        .tips()
        .iter()
        .map(|t| t.event_id.clone())
        .collect();
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::DispositionRecorded {
            finding_id: finding_id.clone(),
            status: disposition,
            commit,
            evidence,
            supersedes,
        },
    );
    store.append_event(&ev)?;
    println!("finding: {finding_id} → {disposition:?}");
    println!("event: {}", ev.event_id);
    Ok(())
}

pub fn review(
    ctx: &Ctx,
    reference: &str,
    verdict: Verdict,
    body: Option<String>,
    patchset: Option<String>,
    findings_json: Option<String>,
    snapshot_first: bool,
) -> Result<()> {
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
        snapshot(ctx, reference, None)?;
    }
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let events = store.load_events(&change_id)?;
    let st = state::reduce(&events)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let patchset_id = resolve_patchset_id(&st, patchset)?
        .context("no patchset to review; run `arc snapshot` first")?;

    let inline: Vec<InlineFinding> = match findings_json {
        None => Vec::new(),
        Some(src) => {
            let text = if src == "-" {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf
            } else {
                std::fs::read_to_string(&src)
                    .with_context(|| format!("cannot read findings file {src}"))?
            };
            let inputs: Vec<FindingInput> =
                serde_json::from_str(&text).context("malformed findings JSON")?;
            inputs
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
                .collect()
        }
    };

    if verdict == Verdict::Approved && inline.iter().any(|f| f.blocking) {
        bail!("cannot approve while recording blocking findings in the same review");
    }

    let finding_ids: Vec<String> = inline.iter().map(|f| f.finding_id.clone()).collect();
    let mut ev = ctx.event(
        &store,
        &change_id,
        Payload::VerdictRecorded {
            patchset_id: patchset_id.clone(),
            verdict,
            body,
            findings: inline,
        },
    );
    ev.event_id = event_id_after(
        &events
            .last()
            .context("change has no opening event")?
            .event_id,
    )?;
    store.append_event(&ev)?;
    println!("verdict: {verdict:?} on {patchset_id}");
    for id in finding_ids {
        println!("finding: {id}");
    }
    println!("event: {}", ev.event_id);
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

fn resolve_patchset_id(st: &ChangeState, patchset: Option<String>) -> Result<Option<String>> {
    match patchset {
        Some(id) => {
            if !st.patchsets.iter().any(|p| p.id == id) {
                bail!("unknown patchset {id:?}");
            }
            Ok(Some(id))
        }
        None => Ok(st.latest_patchset().map(|p| p.id.clone())),
    }
}
