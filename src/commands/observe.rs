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

/// Which members of a watched set must reach a condition before `watch` returns.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WatchQuorum {
    /// Return as soon as one member reaches a condition, naming that member.
    Any,
    /// Return once every member has reached at least one condition.
    All,
}

/// One member that satisfied the watch, with the condition it reached.
struct WatchHit {
    change_id: String,
    condition: WatchUntil,
}

pub fn watch(
    ctx: &Ctx,
    reference: Option<&str>,
    tags: &[String],
    quorum: Option<WatchQuorum>,
    until: &[WatchUntil],
    timeout_secs: Option<u64>,
    exec_command: Option<&str>,
) -> Result<i32> {
    // A single change and a tagged set are different questions, and a quorum is
    // meaningless for one change. Refuse rather than guess, because both wrong
    // guesses — returning early or waiting forever — are silent.
    let selection = match (reference, tags.is_empty()) {
        (Some(_), false) => bail!("<CHANGE> and --tag select different scopes; supply one"),
        (None, true) => bail!("watch requires <CHANGE> or --tag"),
        (Some(reference), true) => {
            if quorum.is_some() {
                bail!("--any and --all apply to --tag, not a single change");
            }
            WatchSelection::Single(ctx.store()?.resolve_change(reference)?)
        }
        (None, false) => {
            let quorum = quorum.context("--tag requires --any or --all")?;
            let tags = normalize_tags(tags.to_vec())?;
            WatchSelection::Tagged(resolve_tagged(ctx, &tags)?, quorum)
        }
    };
    let deadline = timeout_secs.map(|timeout| Instant::now() + Duration::from_secs(timeout));
    let result = gitio::with_deadline(deadline, || {
        watch_until_reached(ctx, &selection, until, deadline)
    });
    match result {
        Ok(Some(hits)) => {
            report_reached(&selection, &hits, until);
            if let Some(command) = exec_command {
                let value = watch_hook_payload(&selection, &hits, until);
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

enum WatchSelection {
    Single(String),
    Tagged(Vec<String>, WatchQuorum),
}

impl WatchSelection {
    fn change_ids(&self) -> &[String] {
        match self {
            WatchSelection::Single(change_id) => std::slice::from_ref(change_id),
            WatchSelection::Tagged(change_ids, _) => change_ids,
        }
    }
}

fn resolve_tagged(ctx: &Ctx, tags: &[String]) -> Result<Vec<String>> {
    let store = ctx.store()?;
    let states = ctx.load_all_states(&store)?;
    let selected = states
        .values()
        .filter(|state| tags.iter().all(|tag| state.tags.contains(tag)))
        .map(|state| state.change_id.clone())
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("no changes match tags {}", tags.join(", "));
    }
    Ok(selected)
}

/// Single-change output stays byte-identical; only tagged watches name members.
fn report_reached(selection: &WatchSelection, hits: &[WatchHit], until: &[WatchUntil]) {
    match selection {
        WatchSelection::Single(_) => println!("reached: {}", hits[0].condition.label()),
        WatchSelection::Tagged(_, WatchQuorum::Any) => {
            println!(
                "reached: {} ({})",
                hits[0].condition.label(),
                hits[0].change_id
            )
        }
        WatchSelection::Tagged(change_ids, WatchQuorum::All) => println!(
            "reached: {} ({} changes)",
            until_labels(until),
            change_ids.len()
        ),
    }
}

fn watch_hook_payload(
    selection: &WatchSelection,
    hits: &[WatchHit],
    until: &[WatchUntil],
) -> serde_json::Value {
    match selection {
        WatchSelection::Tagged(_, WatchQuorum::All) => serde_json::json!({
            "change_id": "",
            "changes": hits.iter().map(|hit| serde_json::json!({
                "change_id": hit.change_id,
                "condition": hit.condition.label(),
            })).collect::<Vec<_>>(),
            "condition": until_labels(until),
            "event_id": "",
            "event_type": "watch-reached",
        }),
        _ => serde_json::json!({
            "change_id": hits[0].change_id,
            "condition": hits[0].condition.label(),
            "event_id": "",
            "event_type": "watch-reached",
        }),
    }
}

fn watch_until_reached(
    ctx: &Ctx,
    selection: &WatchSelection,
    until: &[WatchUntil],
    deadline: Option<Instant>,
) -> Result<Option<Vec<WatchHit>>> {
    let store = ctx.store()?;
    let change_ids = selection.change_ids();
    let quorum = match selection {
        WatchSelection::Single(_) => WatchQuorum::Any,
        WatchSelection::Tagged(_, quorum) => *quorum,
    };
    let mut poll_interval = POLL_MIN;
    loop {
        let mut hits = Vec::new();
        for change_id in change_ids {
            for condition in until {
                if watch_reached(ctx, &store, change_id, *condition)? {
                    hits.push(WatchHit {
                        change_id: change_id.clone(),
                        condition: *condition,
                    });
                    break;
                }
            }
            // One satisfied member is the whole answer under `any`, so stop
            // reducing the rest rather than replaying every member's ledger.
            if quorum == WatchQuorum::Any && !hits.is_empty() {
                return Ok(Some(hits));
            }
        }
        if quorum == WatchQuorum::All && hits.len() == change_ids.len() {
            return Ok(Some(hits));
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
