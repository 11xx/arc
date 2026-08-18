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
    match map.successor(revision) {
        Some(successor) => {
            println!("{revision} → {successor}");
            Ok(0)
        }
        None => {
            println!("{revision}: no recorded rewrite moved it");
            Ok(2)
        }
    }
}
