//! Recording a Git history rewrite, and reading a revision forward through it.

use super::*;
use crate::rewrite::{parse_commit_map, RewriteMap};

/// Record a rewrite the operator performed. arc never rewrites history and
/// never computes the mapping: it is supplied, and the ledger is annotated
/// rather than migrated.
pub fn record_rewrite(ctx: &Ctx, map: &str, reason: String, tool: Option<String>) -> Result<()> {
    let store = ctx.store()?;
    let text = if map == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
        buf
    } else {
        std::fs::read_to_string(map).with_context(|| format!("cannot read commit map {map}"))?
    };
    let mapping = parse_commit_map(&text)?;
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
    let count = mapping.len();
    let event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::HistoryRewritten {
            mapping,
            reason,
            tool,
        },
    );
    store.append_repository_event(&event)?;
    println!("history rewrite recorded: {count} revisions");
    println!("event: {}", event.event_id);
    println!("Recorded revisions still say what they said; readers follow them forward.");
    Ok(())
}

/// Where a recorded revision ended up. Exit 2 when nothing rewrote it, so a
/// script can tell "unchanged" from "moved" without parsing prose.
pub fn resolve_rewritten(ctx: &Ctx, revision: &str) -> Result<i32> {
    let store = ctx.store()?;
    let map = RewriteMap::load(&store)?;
    match map.fate(revision) {
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
