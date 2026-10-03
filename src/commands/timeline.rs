//! Chronological event-log view over a change's ledger.

use super::*;

/// Print one line per recorded event in chronological (ULID) order, or
/// newest-first with `--reverse`. Pure derivation: no writes, no new events.
pub fn log(ctx: &Ctx, reference: &str, reverse: bool) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let mut events = store.load_events(&change_id)?;
    if reverse {
        events.reverse();
    }
    for event in &events {
        println!("{}", render::event_line(event));
        // A finding filed inside a review batch is the same object as one
        // filed standalone, and the batch path is the one review loops use.
        for line in render::nested_finding_lines(event) {
            println!("{line}");
        }
    }
    Ok(())
}
