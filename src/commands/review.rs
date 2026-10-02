//! Patchsets and reviews can be recorded separately or composed safely.
//! A composed review snapshots only a clean, checked-out change worktree so
//! the verdict binds to the exact committed head the reviewer inspected.

use super::*;
use crate::state::{FindingState, VerdictEntry};
use crate::status::{FindingSummary, StatusReport};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Serialize)]
struct ReviewView<'a> {
    schema: &'static str,
    change_id: &'a str,
    verdicts: Vec<ReviewVerdict<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    external_verdicts: Vec<&'a crate::status::ExternalVerdictStatus>,
    open_findings: Vec<&'a FindingSummary>,
    has_valid_approval: bool,
    /// The current status guidance, carried here so a review reader sees the
    /// same available actions as `status` and `inbox`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    review_options: Vec<&'static str>,
    #[serde(skip_serializing_if = "is_false")]
    verdict_contested: bool,
    next_action: &'a str,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize)]
struct ReviewVerdict<'a> {
    verdict: Verdict,
    causes: &'a [ReviewCause],
    patchset_id: &'a str,
    actor: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_behalf_of: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relation: Option<&'a VerdictRelation>,
    created_at: chrono::DateTime<chrono::Utc>,
    valid_for_current_head: bool,
    /// Why this approval was recorded as owed corroboration, when it was.
    /// An approval that gates while owing a second judgment is not the same
    /// object as an unqualified one, and a reader deciding whether the change
    /// is reviewed cannot tell them apart from the rest of this entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    provisional: Option<&'a str>,
    /// Whether this is the provisional approval whose corroboration is still
    /// outstanding. Read from the one derivation of that fact rather than
    /// recomputed, so this view cannot disagree with `arc query --provisional`
    /// or the `check` advisory about the same obligation.
    #[serde(skip_serializing_if = "is_false")]
    provisional_outstanding: bool,
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
        schema: "arc-review/4",
        change_id: &state.change_id,
        verdicts: state
            .verdicts
            .iter()
            .rev()
            .map(|verdict| review_verdict(verdict, &state, &report))
            .collect(),
        external_verdicts: report.external_verdicts.iter().collect(),
        open_findings: report
            .findings
            .iter()
            .filter(|finding| finding.status == "open")
            .collect(),
        has_valid_approval: report.has_valid_approval,
        review_options: report.review_options.clone(),
        verdict_contested: state.verdict_contested(),
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
            // Head validity and what a verdict does to its neighbours are
            // independent facts. Reporting the relation here instead would
            // let a stale corroboration read as though it still covered the
            // head; the relation gets its own line below, and a contested
            // change says so once, about the change.
            if verdict.valid_for_current_head {
                "valid for current head"
            } else {
                "STALE for current head"
            }
        );
        if let Some(reason) = verdict.provisional {
            println!(
                "  - provisional{}: {}",
                if verdict.provisional_outstanding {
                    ", corroboration outstanding"
                } else {
                    ""
                },
                crate::render::one_line(reason)
            );
        }
        if let Some(relation) = verdict.relation {
            println!("  - relation: {}", relation.description());
        }
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

    println!("\n## External verdicts\n");
    if view.external_verdicts.is_empty() {
        println!("No external verdicts recorded.");
    }
    for external in &view.external_verdicts {
        println!(
            "- [external] {:?} at `{}` by {} — {} (reference {})",
            external.verdict,
            external.revision,
            external.decided_by,
            if external.gates_current_head {
                "valid for current head"
            } else if external.matches_current_patchset {
                "applies to current patchset but does not satisfy local review policy"
            } else {
                "stale for current head"
            },
            external.reference
        );
        for finding in &external.findings {
            println!(
                "  - external finding `{}` [{}{:?}] {}",
                finding.finding_id,
                if finding.blocking { "blocking/" } else { "" },
                finding.severity,
                finding.summary
            );
            if let Some(body) = &finding.body {
                println!("    {body}");
            }
        }
    }

    if view.verdict_contested {
        println!("\nVerdict state: CONTESTED");
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
    println!(
        "Review options: {}",
        if view.review_options.is_empty() {
            "none".to_string()
        } else {
            view.review_options.join(", ")
        }
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
        relation: verdict.relation.as_ref(),
        created_at: verdict.created_at,
        valid_for_current_head,
        provisional: verdict.provisional.as_deref(),
        provisional_outstanding: state
            .outstanding_provisional_approval()
            .is_some_and(|outstanding| outstanding.event_id == verdict.event_id),
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

fn contributor_declaration(
    ctx: &Ctx,
    contributors: Option<Vec<String>>,
    solo: bool,
) -> Result<Option<Vec<String>>> {
    if solo && contributors.is_some() {
        bail!("--solo cannot be combined with --contributors");
    }
    if solo {
        let actor = ctx.actor.trim();
        if actor.is_empty() {
            bail!("--solo requires a nonempty invoking actor");
        }
        return Ok(Some(vec![actor.to_string()]));
    }
    let Some(contributors) = contributors else {
        return Ok(None);
    };
    let mut normalized = BTreeSet::new();
    for contributor in contributors {
        let contributor = contributor.trim();
        if contributor.is_empty() {
            bail!("--contributors must name nonempty actors");
        }
        normalized.insert(contributor.to_string());
    }
    if normalized.is_empty() {
        bail!("--contributors must name at least one actor");
    }
    Ok(Some(normalized.into_iter().collect()))
}

/// Say when the commits carry more distinct hands than the declaration does.
///
/// Names are not comparable across the two: a contributor is a declared actor
/// and a Git author is whatever a checkout's config holds, so `codex-luna` and
/// `Ada Lovelace` are the same hand under two namings and no equality test can
/// know it. Comparing them by name warns on every honest snapshot, which is
/// the same as not warning at all.
///
/// Cardinality survives the mismatch. Three Git authors behind one declared
/// contributor means somebody who touched this patchset is not in the set that
/// decides who may review it, and that is worth saying whatever the names are.
fn warn_fewer_contributors_than_hands(ctx: &Ctx, base: &str, head: &str, contributors: &[String]) {
    let range = format!("{base}..{head}");
    let Ok(output) = gitio::git(&ctx.cwd, &["log", "--format=%an <%ae>", &range]) else {
        return;
    };
    let git_authors = output
        .lines()
        .map(str::trim)
        .filter(|author| !author.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    if git_authors.len() > contributors.len() {
        eprintln!(
            "warning: {} distinct Git authors wrote {range} and {} contributor(s) were declared ({}); \
             somebody who touched this patchset is not in the set that decides who may review it",
            git_authors.len(),
            contributors.len(),
            contributors.join(", ")
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub fn snapshot(
    ctx: &Ctx,
    reference: &str,
    base: Option<String>,
    brief_version: Option<usize>,
    contributors: Option<Vec<String>>,
    solo: bool,
    journal_refs: Vec<String>,
    thread: Option<String>,
) -> Result<()> {
    let requested_contributors = contributor_declaration(ctx, contributors, solo)?;
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let events = store.load_events(&change_id)?;
    let mut st = state::reduce_following(&events, &store.rewrites()?)?;
    crate::replica::localize_change(&store.repository_id, &events, &mut st);
    // Snapshotting is the lead's first read of the change's worktree, and an
    // executor confined to that worktree spools its journal writes there.
    // Filing them here puts them in the journal while the worktree still
    // exists to hold them.
    if let Some(worktree) = st.worktree.as_deref() {
        crate::journal::promote_worktree_spool(ctx, std::path::Path::new(worktree));
    }
    // Links are resolved before anything is written, so a name that does not
    // resolve refuses the snapshot instead of recording a dead reference.
    let links_supplied = !journal_refs.is_empty() || thread.is_some();
    let flagged_refs = resolve_journal_refs(ctx, &journal_refs)?;
    let thread = thread
        .map(|thread| parse_thread_reference(&thread))
        .transpose()?;
    let head = gitio::branch_head(&ctx.cwd, &st.branch)?;
    let (default_base, merge_base) = patchset_base(ctx, &st, &head)?;
    let base_rev = match base {
        Some(b) => gitio::rev_parse(&ctx.cwd, &b)?,
        None => default_base,
    };
    let brief = match brief_version {
        Some(0) => bail!("brief version 0 not found"),
        Some(version) => Some(
            st.briefs
                .get(version - 1)
                .with_context(|| format!("brief version {version} not found"))?,
        ),
        None => st.latest_brief(),
    };
    let brief_ref = brief.map(|brief| BriefRef {
        event_id: brief.event_id.clone(),
    });
    let (journal_refs, skipped_defaults) = if flagged_refs.is_empty() {
        default_journal_refs(ctx, st.journal_ref.as_deref(), brief)
    } else {
        (flagged_refs, Vec::new())
    };
    let unchanged_patchset = st
        .latest_patchset()
        .filter(|p| {
            p.head == head
                && p.base == base_rev
                && p.brief_ref == brief_ref
                // Links supplied now are part of what the patchset records: a
                // rerun that adds or changes them is a new patchset. A rerun
                // that supplies none leaves an existing patchset's links in
                // place, defaulted ones included, which is what makes a bare
                // snapshot idempotent.
                && (!links_supplied
                    || (same_journal_refs(&p.journal_refs, &journal_refs)
                        && p.thread == thread))
        })
        .map(|p| p.id.clone());
    let identity = gitio::commit_identity(&ctx.cwd, &head)?;
    let now = chrono::Utc::now();
    let snapshot_claim = st
        .claim
        .as_ref()
        .filter(|claim| state::claim_timing_at(claim, now).active);
    if let Some(claim) = snapshot_claim.filter(|claim| claim.owner.actor != ctx.actor.trim()) {
        if requested_contributors.is_none() {
            bail!(
                "active claim {} is owned by {}; recording a patchset over it requires --contributors or --solo, which `arc snapshot` and `arc done` accept",
                claim.claim_id,
                claim.owner.actor
            );
        }
    }
    let contributors = requested_contributors.clone().unwrap_or_default();
    let patchset_id = format!("ps-{:02}", st.patchsets.len() + 1);
    let unchanged_patchset = unchanged_patchset.filter(|patchset_id| {
        requested_contributors.as_ref().is_none_or(|requested| {
            st.patchsets
                .iter()
                .find(|patchset| patchset.id == *patchset_id)
                .is_some_and(|patchset| patchset.contributors == *requested)
        })
    });
    if let Some(contributors) = requested_contributors.as_deref() {
        warn_fewer_contributors_than_hands(ctx, &base_rev, &head, contributors);
    }
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
        contributors,
        claim_id: snapshot_claim.map(|claim| claim.claim_id.clone()),
        claim_actor: snapshot_claim.map(|claim| claim.owner.actor.clone()),
        journal_refs,
        thread,
        candidate: None,
    };
    ensure_append_allowed(&st, &payload)?;
    if let Some(patchset_id) = unchanged_patchset {
        println!("patchset: {patchset_id} (unchanged)");
        return Ok(());
    }
    for skipped in &skipped_defaults {
        eprintln!("{skipped}");
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
    if !payload_contributors(&ev.payload).is_empty() {
        println!(
            "contributors: {}",
            payload_contributors(&ev.payload).join(",")
        );
    }
    println!("event: {}", ev.event_id);
    Ok(())
}

/// The base a patchset at `head` is recorded against when none is named,
/// and the merge base with the target it was read from.
pub(super) fn patchset_base(
    ctx: &Ctx,
    st: &ChangeState,
    head: &str,
) -> Result<(String, Option<String>)> {
    let merge_base = gitio::branch_head(&ctx.cwd, &st.target_branch)
        .ok()
        .and_then(|target_head| gitio::merge_base(&ctx.cwd, &target_head, head).ok());
    let base = match merge_base.as_ref() {
        Some(merge_base) => {
            // A stacked change's opening base is a floor: use it only while it lies
            // between the target merge base and the branch head.
            let opening_base_is_floor = merge_base != &st.base
                && gitio::is_ancestor(&ctx.cwd, merge_base, &st.base)?
                && gitio::is_ancestor(&ctx.cwd, &st.base, head)?;
            if opening_base_is_floor {
                st.base.clone()
            } else {
                merge_base.clone()
            }
        }
        None => st.base.clone(),
    };
    Ok((base, merge_base))
}

/// Turn `--journal-ref` filenames into the references a patchset records,
/// reading each body for the digest.
///
/// A filename is resolved against the hot journal and its cold archive before
/// anything is written; a name that resolves to no artifact is refused. One
/// filename given twice is refused rather than recorded twice, because the
/// second would say nothing the first did not.
fn resolve_journal_refs(ctx: &Ctx, files: &[String]) -> Result<Vec<JournalArtifactRef>> {
    let mut seen = BTreeSet::new();
    let mut refs = Vec::new();
    for file in files {
        if !seen.insert(file.as_str()) {
            bail!("--journal-ref {file:?} was given more than once");
        }
        refs.push(crate::journal::artifact_reference(
            ctx,
            file,
            JournalRefVia::Flag,
        )?);
    }
    Ok(refs)
}

/// The journal links a snapshot records when none is flagged: the artifact
/// the change was opened from, then the plan the brief names, one link per
/// file and each with the digest read now.
///
/// A file named by both is the change's opening framing, so it is recorded
/// once as `begin`. A default that no longer resolves is not recorded; the
/// second value holds one warning line per such file, naming its source.
pub(super) fn default_journal_refs(
    ctx: &Ctx,
    opened_from: Option<&str>,
    brief: Option<&state::Brief>,
) -> (Vec<JournalArtifactRef>, Vec<String>) {
    let candidates = [
        (opened_from, JournalRefVia::Begin, "begin --from-journal"),
        (
            brief.and_then(|brief| brief.plan_ref.as_deref()),
            JournalRefVia::Brief,
            "the brief's plan reference",
        ),
    ];
    let mut refs: Vec<JournalArtifactRef> = Vec::new();
    let mut skipped = Vec::new();
    for (file, via, source) in candidates {
        let Some(file) = file else { continue };
        if refs.iter().any(|link| link.file == file) {
            continue;
        }
        match crate::journal::artifact_reference(ctx, file, via) {
            Ok(link) => refs.push(link),
            Err(err) => skipped.push(format!(
                "warning: journal link {file} from {source} not recorded: {err:#}"
            )),
        }
    }
    (refs, skipped)
}

/// Whether two link sets name the same artifacts at the same digests. Where a
/// link came from is not part of what it names.
fn same_journal_refs(a: &[JournalArtifactRef], b: &[JournalArtifactRef]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a.file == b.file && a.digest == b.digest)
}

/// Parse `SCHEME:ID` into the identifiers a patchset records.
///
/// The pair is opaque: arc validates its shape and stores it, and never
/// fetches, resolves, or reads anything behind it. An empty half is refused
/// because a scheme without an id names no thread, and an id without a scheme
/// does not say where to look.
fn parse_thread_reference(value: &str) -> Result<ExternalThreadRef> {
    let Some((scheme, id)) = value.split_once(':') else {
        bail!("--thread must be SCHEME:ID, e.g. t3:thread-123");
    };
    let scheme = scheme.trim();
    let id = id.trim();
    if scheme.is_empty() || id.is_empty() {
        bail!("--thread must name both a scheme and an id, e.g. t3:thread-123");
    }
    let valid_scheme = scheme
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid_scheme {
        bail!(
            "--thread scheme {scheme:?} is not a scheme name (a letter followed by letters, \
             digits, +, -, or .)"
        );
    }
    if id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("--thread id must not contain whitespace or control characters");
    }
    Ok(ExternalThreadRef {
        scheme: scheme.to_string(),
        id: id.to_string(),
    })
}

fn payload_contributors(payload: &Payload) -> &[String] {
    match payload {
        Payload::PatchsetAdded { contributors, .. } => contributors,
        _ => &[],
    }
}

pub fn amend_attribution(
    ctx: &Ctx,
    reference: &str,
    patchset: String,
    contributors: Option<Vec<String>>,
    solo: bool,
) -> Result<()> {
    let contributors = contributor_declaration(ctx, contributors, solo)?
        .context("--amend requires --contributors or --solo")?;
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let events = store.load_events(&change_id)?;
    let mut st = state::reduce_following(&events, &store.rewrites()?)?;
    crate::replica::localize_change(&store.repository_id, &events, &mut st);
    let patchset_id = resolve_patchset_id(&st, Some(patchset))?
        .context("no patchset to amend; run `arc snapshot` first")?;
    let patchset = st
        .patchsets
        .iter()
        .find(|patchset| patchset.id == patchset_id)
        .with_context(|| format!("unknown patchset {patchset_id}"))?;
    // The refusal names the concrete blocker from loaded state — the first
    // verdict on this patchset, or the terminal closure — rather than
    // attempting a mutation that the append policy will refuse anyway.
    if let Some(verdict) = st
        .verdicts
        .iter()
        .find(|verdict| verdict.patchset_id == patchset_id)
    {
        bail!(
            "patchset {patchset_id} attribution cannot be amended after verdict {} by {}; \
             attribution is repairable only before any verdict, because a verdict binds to \
             the authorship it judged",
            verdict.event_id,
            verdict.effective_author(),
        );
    }
    if st.closure.is_some() {
        let outcome = match st.closure.as_ref().map(|closure| closure.outcome) {
            Some(Closure::Integrated) => "integrated",
            Some(Closure::Abandoned) => "abandoned",
            Some(Closure::Superseded) => "superseded",
            None => "closed",
        };
        bail!(
            "patchset {patchset_id} attribution cannot be amended after the change is {outcome}; \
             the shipped revision keeps the authorship that authorized it, and the unresolved \
             identities stay recorded as debt with the exact patchset"
        );
    }
    warn_fewer_contributors_than_hands(ctx, &patchset.base, &patchset.head, &contributors);
    let payload = Payload::PatchsetAttributionAmended {
        patchset_id: patchset_id.clone(),
        contributors: contributors.clone(),
    };
    ensure_append_allowed(&st, &payload)?;
    let mut event = ctx.event(&store, &change_id, payload);
    event.event_id = event_id_after(
        &events
            .last()
            .context("change has no opening event")?
            .event_id,
    )?;
    store.append_event(&event)?;
    println!("patchset: {patchset_id}");
    println!("contributors: {}", contributors.join(","));
    println!("event: {}", event.event_id);
    Ok(())
}

/// Keep one fact the work discovered, so `resume` hands it back.
///
/// Deliberately cheap: one line, no artifact, no ceremony. Selectivity has to
/// be cheaper than completeness, or this becomes a second transcript and
/// reproduces the problem it exists to solve at higher cost.
pub fn keep(
    ctx: &Ctx,
    reference: &str,
    kind: crate::model::KeptKind,
    body: String,
    evidence: Option<String>,
    cites: Vec<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, _st) = ctx.load_state(&store, reference)?;
    let cites = validate_citations(&store, &change_id, cites)?;
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::ContextKept {
            kind,
            body,
            evidence,
            cites,
        },
    );
    store.append_event(&ev)?;
    println!("kept: {} {}", kind.as_str(), ev.event_id);
    Ok(())
}

/// The cited ids in order, each once. Every id must name an event on this
/// change that a kept fact can rest on: a verification, a verdict (local,
/// external, or audit), a finding, a disposition, or an earlier kept fact.
fn validate_citations(store: &Store, change_id: &str, cites: Vec<String>) -> Result<Vec<String>> {
    if cites.is_empty() {
        return Ok(cites);
    }
    let events = store.load_events(change_id)?;
    let mut recorded = Vec::new();
    for requested in cites {
        crate::ids::validate_id_component(&requested)?;
        let event = events
            .iter()
            .find(|event| event.event_id == requested)
            .with_context(|| {
                format!(
                    "no event {requested:?} on change {change_id:?}; --cites must name an event on this change"
                )
            })?;
        if !matches!(
            &event.payload,
            Payload::VerificationRecorded { .. }
                | Payload::VerificationReused { .. }
                | Payload::VerdictRecorded { .. }
                | Payload::ExternalVerdictRecorded { .. }
                | Payload::AuditVerdictRecorded { .. }
                | Payload::FindingAdded { .. }
                | Payload::AuditFindingAdded { .. }
                | Payload::DispositionRecorded { .. }
                | Payload::AuditDispositionRecorded { .. }
                | Payload::ContextKept { .. }
        ) {
            let actual = crate::render::event_kind_summary(&event.payload).0;
            bail!(
                "event {requested} is a {actual}, which a kept fact cannot cite; cite a \
                 verification, verdict, finding, disposition, or kept fact"
            );
        }
        if !recorded.contains(&requested) {
            recorded.push(requested);
        }
    }
    Ok(recorded)
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

fn validate_evidence_event_id(store: &Store, change_id: &str, requested: &str) -> Result<String> {
    crate::ids::validate_id_component(requested)?;
    let event = store
        .load_events(change_id)?
        .into_iter()
        .find(|event| event.event_id == requested)
        .with_context(|| {
            format!(
                "no event {requested:?} on change {change_id:?}; --evidence-event must name an event on this change"
            )
        })?;
    if !matches!(
        &event.payload,
        Payload::VerificationRecorded { .. } | Payload::VerificationReused { .. }
    ) {
        let actual = crate::render::event_kind_summary(&event.payload).0;
        bail!("event {requested} is a {actual}, not a verification");
    }
    Ok(event.event_id)
}

pub fn resolve(
    ctx: &Ctx,
    reference: &str,
    finding: String,
    disposition: DispositionStatus,
    commit: Option<String>,
    evidence: Option<String>,
    evidence_event_id: Option<String>,
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
    let evidence_event_id = evidence_event_id
        .as_deref()
        .map(|requested| validate_evidence_event_id(&store, &change_id, requested))
        .transpose()?;
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
            evidence_event_id,
            supersedes,
        }
    } else {
        Payload::DispositionRecorded {
            finding_id: finding_id.clone(),
            status: disposition,
            commit,
            evidence,
            evidence_event_id,
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
    pub relation: VerdictRelationKind,
    pub body: Option<String>,
    pub provisional: Option<String>,
    pub patchset: Option<String>,
    pub causes: Vec<ReviewCause>,
    pub findings_json: Option<String>,
    pub snapshot_first: bool,
    /// The routing version that selected the reviewer, as the caller declared
    /// it. `None` records an unrouted review.
    pub route_version: Option<String>,
}

pub fn review(ctx: &Ctx, reference: &str, args: ReviewArgs) -> Result<()> {
    let ReviewArgs {
        verdict,
        relation,
        body,
        provisional,
        patchset,
        mut causes,
        findings_json,
        snapshot_first,
        route_version,
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
        snapshot(ctx, reference, None, None, None, false, Vec::new(), None)?;
    }
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let events = store.load_events(&change_id)?;
    let mut st = state::reduce_following(&events, &store.rewrites()?)?;
    crate::replica::localize_change(&store.repository_id, &events, &mut st);
    let patchset_id = resolve_patchset_id(&st, patchset)?
        .context("no patchset to review; run `arc snapshot` first")?;
    let observed: Vec<String> = st
        .verdict_tips()
        .into_iter()
        .map(|verdict| verdict.event_id.clone())
        .collect();
    let relation = (!observed.is_empty()).then(|| relation.with_observed(observed));

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
    if let Some(relation) = relation.as_ref() {
        println!("relation: {}", relation.description());
    }
    let payload = Payload::VerdictRecorded {
        patchset_id: patchset_id.clone(),
        verdict,
        causes,
        body,
        findings: inline,
        relation,
        provisional: provisional.clone(),
        route_version,
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
    // The inert-approval note already names the reviewer's relation to the
    // work when the gate rejects it. Where the gate lets the approval stand —
    // a repository that permits self-approval, or a change outside the
    // dangerous surfaces — the same relation is still what a reader needs to
    // judge the verdict, so it is said rather than left to be inferred.
    if !report_inert_approval(ctx, &store, &change_id)? {
        if let Some(patchset) = st.patchsets.iter().find(|p| p.id == patchset_id) {
            ctx.warn_verdict_not_independent(patchset, verdict);
        }
    }
    Ok(())
}

/// Say so when the approval just recorded cannot gate.
///
/// Appending it is correct — a verdict is a fact about what someone concluded,
/// not a request for permission. Reporting success for an act with no effect
/// is what teaches an operator the guard is absent, so the write path
/// evaluates the same policy `check` does and names the outcome on the spot.
///
/// Reports whether it said anything, so a caller can tell an approval the gate
/// rejected from one it let stand.
fn report_inert_approval(ctx: &Ctx, store: &Store, change_id: &str) -> Result<bool> {
    let st = store.state(change_id)?;
    let report = ctx.report(store, &st)?;
    let Some(reason) = report.approval_rejection_reason.as_deref() else {
        return Ok(false);
    };
    println!("note: this approval does not gate — {reason}.");
    println!(
        "      integration will still refuse. Record the review this change owes with \
`arc integrate {change_id} --debt <reason>`, or obtain a verdict from a \
different actor."
    );
    Ok(true)
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

/// The fields `FindingInput` reads, in the order `--help` names them.
const FINDING_FIELDS: [&str; 5] = ["blocking", "severity", "summary", "body", "anchor"];

/// Folded field names taken for a finding field beyond a one-edit typo.
const FINDING_FIELD_ALIASES: [(&str, &str); 10] = [
    ("block", "blocking"),
    ("blocked", "blocking"),
    ("blocker", "blocking"),
    ("blockers", "blocking"),
    ("blocks", "blocking"),
    ("isblocking", "blocking"),
    ("severitylevel", "severity"),
    ("title", "summary"),
    ("file", "anchor"),
    ("path", "anchor"),
];

/// Read a findings batch from a file or stdin.
pub(crate) fn read_finding_inputs(src: &str) -> Result<Vec<FindingInput>> {
    let text = if src == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        std::fs::read_to_string(src).with_context(|| format!("cannot read findings file {src}"))?
    };
    for warning in unknown_finding_fields(&text)? {
        eprintln!("warning: {warning}");
    }
    serde_json::from_str(&text).context("malformed findings JSON")
}

/// Refuses a finding whose unknown field looks like a misspelling of a field
/// it omits, because the omitted field would be recorded at its default — a
/// misspelled `blocking` records a non-blocking finding. Returns a warning for
/// each finding carrying any other unknown field. `id` is accepted silently:
/// arc assigns finding IDs and ignores a supplied one.
fn unknown_finding_fields(text: &str) -> Result<Vec<String>> {
    let Ok(serde_json::Value::Array(findings)) = serde_json::from_str(text) else {
        return Ok(Vec::new());
    };
    let accepted = FINDING_FIELDS.join(", ");
    let mut warnings = Vec::new();
    for (position, finding) in (1..).zip(&findings) {
        let Some(finding) = finding.as_object() else {
            continue;
        };
        let mut unknown = Vec::new();
        for key in finding.keys() {
            if key == "id" || FINDING_FIELDS.contains(&key.as_str()) {
                continue;
            }
            if let Some(field) = misspelled_field(key).filter(|field| !finding.contains_key(*field))
            {
                bail!(
                    "finding {position} has unknown field `{key}`, which looks like a misspelling of `{field}`; rename or remove it (a finding reads {accepted})"
                );
            }
            unknown.push(format!("`{key}`"));
        }
        if !unknown.is_empty() {
            let noun = if unknown.len() == 1 {
                "field"
            } else {
                "fields"
            };
            warnings.push(format!(
                "finding {position} has unknown {noun} {}, which arc ignores (a finding reads {accepted})",
                unknown.join(", ")
            ));
        }
    }
    Ok(warnings)
}

/// The finding field `key` names once case, `_`, and `-` are disregarded,
/// through an alias or within one edit.
fn misspelled_field(key: &str) -> Option<&'static str> {
    let folded: String = key
        .chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    FINDING_FIELD_ALIASES
        .iter()
        .find(|(alias, _)| *alias == folded)
        .map(|(_, field)| *field)
        .or_else(|| {
            FINDING_FIELDS
                .into_iter()
                .find(|field| within_one_edit(&folded, field))
        })
}

/// Whether `a` becomes `b` by at most one insertion, deletion, substitution,
/// or swap of adjacent characters.
fn within_one_edit(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let (a, b) = (&a[common..], &b[common..]);
    let swapped =
        a.len() >= 2 && a.len() == b.len() && a[0] == b[1] && a[1] == b[0] && a[2..] == b[2..];
    a == b
        || a.get(1..) == Some(b)
        || b.get(1..) == Some(a)
        || a.get(1..).is_some_and(|rest| b.get(1..) == Some(rest))
        || swapped
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

#[cfg(test)]
mod tests {
    use super::misspelled_field;

    #[test]
    fn a_misspelled_finding_field_is_named_and_an_unrelated_one_is_not() {
        for (key, field) in [
            ("blocker", "blocking"),
            ("Blocking", "blocking"),
            ("is_blocking", "blocking"),
            ("bolcking", "blocking"),
            ("severity_level", "severity"),
            ("sevrity", "severity"),
            ("title", "summary"),
            ("summery", "summary"),
            ("bdoy", "body"),
            ("path", "anchor"),
            ("anchors", "anchor"),
        ] {
            assert_eq!(misspelled_field(key), Some(field), "{key}");
        }
        for key in ["tool", "code", "author", "rule", "finding_id", "url"] {
            assert_eq!(misspelled_field(key), None, "{key}");
        }
    }
}
