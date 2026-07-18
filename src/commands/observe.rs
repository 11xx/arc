use super::*;

/// Replay raw ledger events as compact NDJSON, optionally continuing as new
/// event files arrive. Event IDs are ULIDs, so replay and each observed polling
/// batch can be sorted across changes; concurrent appends may cross batches.
pub fn events(
    ctx: &Ctx,
    follow: bool,
    change: Option<&str>,
    event_type: Option<&str>,
) -> Result<()> {
    let store = ctx.store()?;
    let change_id = change
        .map(|reference| store.resolve_change(reference))
        .transpose()?;
    let mut seen = BTreeSet::new();
    let mut poll_interval = POLL_MIN;

    loop {
        let raw_events = match &change_id {
            Some(id) => store.raw_events_unseen(id, &seen)?,
            None => store.raw_events_all_unseen(&seen)?,
        };
        let observed_events = !raw_events.is_empty();
        let mut out = std::io::stdout().lock();
        for (event_id, value) in raw_events {
            seen.insert(event_id);
            if !event_type.is_none_or(|wanted| {
                value.get("event_type").and_then(serde_json::Value::as_str) == Some(wanted)
            }) {
                continue;
            }
            serde_json::to_writer(&mut out, &value)?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
        if !follow {
            return Ok(());
        }
        poll_interval = if observed_events {
            POLL_MIN
        } else {
            (poll_interval * 2).min(POLL_MAX)
        };
        thread::sleep(poll_interval);
    }
}

pub fn watch(
    ctx: &Ctx,
    reference: &str,
    until: WatchUntil,
    timeout_secs: Option<u64>,
) -> Result<i32> {
    let deadline = timeout_secs.map(|timeout| Instant::now() + Duration::from_secs(timeout));
    let result = gitio::with_deadline(deadline, || {
        watch_until_reached(ctx, reference, until, deadline)
    });
    match result {
        Ok(true) => {
            println!("reached: {}", until.label());
            Ok(0)
        }
        Ok(false) => {
            println!("timeout: {}", until.label());
            Ok(2)
        }
        Err(_) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
            println!("timeout: {}", until.label());
            Ok(2)
        }
        Err(error) => Err(error),
    }
}

fn watch_until_reached(
    ctx: &Ctx,
    reference: &str,
    until: WatchUntil,
    deadline: Option<Instant>,
) -> Result<bool> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let mut poll_interval = POLL_MIN;
    loop {
        if watch_reached(ctx, &store, &change_id, until)? {
            return Ok(true);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(false);
        }
        // Poll the derived condition itself rather than gating checks on event
        // discovery. `ready` also depends on live Git state. Backoff keeps idle
        // watchers cheap while retaining sub-second response for fresh work.
        let sleep_for = deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .map_or(poll_interval, |remaining| poll_interval.min(remaining));
        thread::sleep(sleep_for);
        poll_interval = (poll_interval * 2).min(POLL_MAX);
    }
}

fn watch_reached(ctx: &Ctx, store: &Store, change_id: &str, until: WatchUntil) -> Result<bool> {
    let events = store.load_events(change_id)?;
    let state = state::reduce(&events)?;
    Ok(match until {
        WatchUntil::Snapshot => state.latest_patchset().is_some(),
        WatchUntil::Stalled => state
            .claim
            .as_ref()
            .is_some_and(|claim| state::claim_timing_at(claim, chrono::Utc::now()).stale),
        WatchUntil::Ready => ctx.report(store, &state)?.integrate_ready,
        WatchUntil::Integrated => state
            .closure
            .as_ref()
            .is_some_and(|closure| closure.outcome == Closure::Integrated),
        WatchUntil::Closed => state.is_closed(),
    })
}
