//! Raw event replay/follow with resumable cursors and shell hooks, plus
//! polling watches that can wait for the first of several conditions.

use super::*;

/// Replay raw ledger events as compact NDJSON, optionally continuing as new
/// event files arrive. Event IDs are ULIDs, so replay and each observed polling
/// batch can be sorted across changes; concurrent appends may cross batches.
pub fn events(
    ctx: &Ctx,
    follow: bool,
    change: Option<&str>,
    event_type: Option<&str>,
    since: Option<ulid::Ulid>,
    exec_command: Option<&str>,
) -> Result<()> {
    let store = ctx.store()?;
    let change_id = change
        .map(|reference| store.resolve_change(reference))
        .transpose()?;
    let mut seen = BTreeSet::new();
    let mut poll_interval = POLL_MIN;
    let since = since.map(|cursor| cursor.to_string());

    loop {
        let raw_events = match &change_id {
            Some(id) => store.raw_events_unseen(id, &seen)?,
            None => store.raw_events_all_unseen(&seen)?,
        };
        let observed_events = !raw_events.is_empty();
        let mut out = std::io::stdout().lock();
        for (event_id, value) in raw_events {
            seen.insert(event_id.clone());
            if since
                .as_deref()
                .is_some_and(|cursor| event_id.as_str() <= cursor)
            {
                continue;
            }
            if !event_type.is_none_or(|wanted| {
                value.get("event_type").and_then(serde_json::Value::as_str) == Some(wanted)
            }) {
                continue;
            }
            let mut line = serde_json::to_vec(&value)?;
            line.push(b'\n');
            out.write_all(&line)?;
            out.flush()?;
            if let Some(command) = exec_command {
                run_hook(command, &line, &value);
            }
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
    until: &[WatchUntil],
    timeout_secs: Option<u64>,
    exec_command: Option<&str>,
) -> Result<i32> {
    let deadline = timeout_secs.map(|timeout| Instant::now() + Duration::from_secs(timeout));
    let result = gitio::with_deadline(deadline, || {
        watch_until_reached(ctx, reference, until, deadline)
    });
    match result {
        Ok(Some(condition)) => {
            println!("reached: {}", condition.label());
            if let Some(command) = exec_command {
                let value = serde_json::json!({
                    "change_id": ctx.store()?.resolve_change(reference)?,
                    "condition": condition.label(),
                    "event_id": "",
                    "event_type": "watch-reached",
                });
                let mut diagnostic = serde_json::to_vec(&value)?;
                diagnostic.push(b'\n');
                run_hook(command, &diagnostic, &value);
            }
            Ok(0)
        }
        Ok(None) => {
            println!("timeout: {}", until_labels(until));
            Ok(2)
        }
        Err(_) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
            println!("timeout: {}", until_labels(until));
            Ok(2)
        }
        Err(error) => Err(error),
    }
}

fn watch_until_reached(
    ctx: &Ctx,
    reference: &str,
    until: &[WatchUntil],
    deadline: Option<Instant>,
) -> Result<Option<WatchUntil>> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let mut poll_interval = POLL_MIN;
    loop {
        for condition in until {
            if watch_reached(ctx, &store, &change_id, *condition)? {
                return Ok(Some(*condition));
            }
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(None);
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

fn until_labels(until: &[WatchUntil]) -> String {
    until
        .iter()
        .map(|condition| condition.label())
        .collect::<Vec<_>>()
        .join(",")
}

fn run_hook(command: &str, input: &[u8], value: &serde_json::Value) {
    let event_id = value
        .get("event_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let event_type = value
        .get("event_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let change_id = value
        .get("change_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let result = std::process::Command::new("sh")
        .args(["-c", command])
        .env("ARC_EVENT_ID", event_id)
        .env("ARC_EVENT_TYPE", event_type)
        .env("ARC_CHANGE_ID", change_id)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child.stdin.take().unwrap().write_all(input)?;
            child.wait()
        });
    match result {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("warning: event hook exited with {status}"),
        Err(error) => eprintln!("warning: event hook failed: {error}"),
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
