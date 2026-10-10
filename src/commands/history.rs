//! Recording a Git history rewrite, and reading a revision forward through it.

use super::*;
use crate::operation_println as println;
use crate::rewrite::{parse_commit_map, RewriteMap};

/// Record a rewrite performed elsewhere, from a supplied commit map. The
/// ledger is annotated rather than migrated: nothing already written changes.
/// The commits the ledger cites that the map replaces are held by retention
/// refs while they are still here, since the tool that rewrote the history
/// has already moved what held them.
pub fn record_rewrite(ctx: &Ctx, map: &str, reason: String, tool: Option<String>) -> Result<()> {
    let text = if map == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
        buf
    } else {
        std::fs::read_to_string(map).with_context(|| format!("cannot read commit map {map}"))?
    };
    let mapping = parse_commit_map(&text)?;
    let event_id = record_mapping(ctx, mapping, reason, tool, BTreeMap::new())?;
    println!("event: {event_id}");
    println!("Recorded revisions still say what they said; readers follow them forward.");
    Ok(())
}

/// Record a mapping as a rewrite of this repository's history, and answer
/// with the event that holds it.
///
/// Computed and supplied maps use [`prepare_mapping`] to validate their
/// successors, consistency and signature-only approval equivalence.
pub fn record_mapping(
    ctx: &Ctx,
    mapping: std::collections::BTreeMap<String, Option<String>>,
    reason: String,
    tool: Option<String>,
    refs: BTreeMap<String, crate::rewrite::RefMove>,
) -> Result<String> {
    let store = ctx.store()?;
    // The same lock the import path takes: a rewrite recorded here and one
    // arriving in a bundle build the same map, and it is judged as a whole.
    let _repository_events = store.lock_repository_events()?;
    let count = mapping.len();
    let retention = crate::retention::plan(&ctx.cwd, &store, &mapping, &refs)?;
    let event = prepare_mapping(
        ctx,
        &store,
        mapping,
        reason,
        tool,
        refs,
        retention.retain.clone(),
    )?;
    // Retention precedes publication and is withdrawn only when the event
    // is confirmed absent; an uncertain publication may already cite it.
    let (held, _) = crate::retention::creations(&ctx.cwd, &retention.retain)?;
    gitio::update_refs(&ctx.cwd, &held)?;
    if let Err(error) = store.append_repository_event(&event) {
        match store.repository_event_is_published(&event) {
            Ok(false) => {
                gitio::update_refs(&ctx.cwd, &crate::retention::withdrawals(&held))
                    .with_context(|| format!(
                        "history rewrite publication failed ({error:#}); retention refs could not be withdrawn"
                    ))?;
                return Err(error.context("history rewrite event is absent; retention refs from this attempt were withdrawn"));
            }
            Ok(true) => return Err(error.context(format!(
                "history rewrite event {} is published; retention refs are preserved",
                event.event_id
            ))),
            Err(inspection) => return Err(error.context(format!(
                "history rewrite event {} publication is uncertain ({inspection:#}); retention refs are preserved",
                event.event_id
            ))),
        }
    }
    println!("history rewrite recorded: {count} revisions");
    for (name, commit) in &retention.retain {
        println!("retained: {name} {commit}");
    }
    super::rewrite::report_unretained(&retention.gone);
    Ok(event.event_id)
}

/// Validate and construct a mapping event under the repository-event lock.
/// Ref-moving callers prepare it before applying their transaction.
pub(super) fn prepare_mapping(
    ctx: &Ctx,
    store: &Store,
    mapping: BTreeMap<String, Option<String>>,
    reason: String,
    tool: Option<String>,
    refs: BTreeMap<String, crate::rewrite::RefMove>,
    retained: BTreeMap<String, String>,
) -> Result<crate::model::Event> {
    // A map from another repository, or with a typo in it, would otherwise be
    // recorded as fact: doctor would report a rewritten revision and suppress
    // the dangling warning, while nothing could resolve the successor. arc
    // does not verify that the rewrite happened — it verifies that what the
    // map claims survives is actually here.
    // Present is not enough: a successor has to be a commit. A map naming a
    // blob or tree that happens to exist would be recorded, and `diff` would
    // then follow a recorded revision into something Git refuses to diff.
    let mut unusable = Vec::new();
    for successor in mapping.values().filter_map(|new| new.as_deref()) {
        if crate::gitio::rev_parse(&ctx.cwd, &format!("{successor}^{{commit}}")).is_err() {
            unusable.push(successor.to_string());
        }
    }
    if !unusable.is_empty() {
        bail!(
            "{} of the mapped revisions are not commits in this repository ({}); this map does \
             not describe this repository's history",
            unusable.len(),
            unusable
                .iter()
                .take(3)
                .map(|revision| revision[..revision.len().min(8)].to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    // Recording is judged against what is already held, for the same reason
    // an import is: two rewrites can disagree about one revision.
    let mut combined = store.load_repository_events()?;
    let mut event = ctx.event(
        store,
        Store::REPOSITORY_SCOPE,
        Payload::HistoryRewritten {
            mapping,
            reason,
            tool,
            signature_only: Vec::new(),
            refs,
            retained,
        },
    )?;
    combined.push(event.clone());
    combined.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    let rewrites = RewriteMap::from_events(combined.iter())
        .context("this rewrite contradicts one already recorded; nothing was written")?;
    // Judged here, while the old commits still exist: an approval follows a
    // rewrite only when this says the successor differs by signature alone.
    if let Payload::HistoryRewritten {
        mapping,
        signature_only,
        ..
    } = &mut event.payload
    {
        *signature_only = crate::provisional::retain_signature_only(
            &ctx.cwd,
            Some(store),
            &[],
            &rewrites,
            crate::rewrite::signature_only_successors(&ctx.cwd, mapping, &rewrites),
        )?;
    }
    Ok(event)
}

/// Withdraw a map as a repository fact, preserving its record and its Git
/// effects. Existing ref-move details remain available for manual recovery.
pub fn withdraw_rewrite(ctx: &Ctx, event_id: &str, reason: String) -> Result<()> {
    crate::ids::validate_id_component(event_id)?;
    if reason.trim().is_empty() {
        bail!("a withdrawal needs a non-empty reason");
    }
    let store = ctx.store()?;
    let _repository_events = store.lock_repository_events()?;
    let events = store.load_repository_events()?;
    let target = events
        .iter()
        .find(|event| event.event_id == event_id)
        .with_context(|| format!("{event_id} is not a history-rewritten repository event"))?;
    let Payload::HistoryRewritten { refs, retained, .. } = &target.payload else {
        bail!("{event_id} is not a history-rewritten repository event");
    };
    if target.change_id != Store::REPOSITORY_SCOPE {
        bail!("{event_id} is not a history-rewritten repository event");
    }
    if let Some(existing) = events.iter().find(|event| {
        matches!(
            &event.payload, Payload::HistoryRewriteWithdrawn { rewrite_event_id, .. }
                if rewrite_event_id == event_id
        )
    }) {
        println!("{event_id} was already withdrawn by {}", existing.event_id);
    } else {
        let event = ctx.event(
            &store,
            Store::REPOSITORY_SCOPE,
            Payload::HistoryRewriteWithdrawn {
                rewrite_event_id: event_id.to_string(),
                reason,
            },
        )?;
        store.append_repository_event(&event)?;
        println!("withdrawn: {event_id}");
        println!("event: {}", event.event_id);
    }
    println!("No refs were moved back.");
    if refs.is_empty() {
        println!("Ref moves were not recorded for this map.");
    } else {
        for (name, moved) in refs {
            println!("withdrawn map moved: {name} {} → {}", moved.old, moved.new);
        }
    }
    // The commits the map replaced stay held: whether the map was right does
    // not change that the ledger cited them.
    for (name, commit) in retained {
        println!("withdrawn map retained: {name} {commit}; kept");
    }
    Ok(())
}

/// Where a recorded revision ended up. Exit 2 when nothing rewrote it, so a
/// script can tell "unchanged" from "moved" without parsing prose.
pub fn resolve_rewritten(ctx: &Ctx, revision: &str) -> Result<i32> {
    let store = ctx.store()?;
    let map = RewriteMap::load(&store)?;
    match map.fate(revision)? {
        Some(crate::rewrite::Fate::Rewritten(successor)) => {
            println!("{revision} → {successor}");
            Ok(0)
        }
        Some(crate::rewrite::Fate::Dropped) => {
            println!("{revision}: dropped by a recorded rewrite; nothing survives it");
            Ok(3)
        }
        None => {
            println!("{revision}: no recorded rewrite moved it");
            Ok(2)
        }
    }
}
