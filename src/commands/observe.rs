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
    tags: &[String],
    event_type: Option<&str>,
    since: Option<ulid::Ulid>,
    exec_command: Option<&str>,
) -> Result<()> {
    if change.is_some() && !tags.is_empty() {
        bail!("--change and --tag select different scopes; supply one");
    }
    let store = ctx.store()?;
    let change_id = change
        .map(|reference| store.resolve_change(reference))
        .transpose()?;
    // A tagged program is the unit an orchestrator waits on, and following
    // each member separately loses the interleaving that makes the stream
    // worth reading. Membership is re-derived each pass, so a change that
    // acquires the tag mid-follow joins the stream — which is what "the
    // changes carrying this tag" means while it is being followed.
    let tags = if tags.is_empty() {
        Vec::new()
    } else {
        normalize_tags(tags.to_vec())?
    };
    let mut seen = BTreeSet::new();
    let mut poll_interval = POLL_MIN;
    let since = since.map(|cursor| cursor.to_string());

    loop {
        let tagged: Option<BTreeSet<String>> = if tags.is_empty() {
            None
        } else {
            Some(resolve_tagged(ctx, &tags)?.into_iter().collect())
        };
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
            if let Some(members) = &tagged {
                let belongs = value
                    .get("change_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| members.contains(id));
                if !belongs {
                    continue;
                }
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

/// One member that satisfied the watch, with the condition it reached and the
/// event that made it true.
///
/// Some conditions are a fact somebody recorded — a snapshot, a closure — and
/// name the event that recorded it. Others are derived from elapsed time or
/// from policy, and no event made them true; those say so rather than naming
/// the newest event and implying a causal link that is not there.
struct WatchHit {
    change_id: String,
    condition: WatchUntil,
    event_id: Option<String>,
}

pub struct WatchArgs<'a> {
    pub tags: &'a [String],
    pub quorum: Option<WatchQuorum>,
    pub until: &'a [WatchUntil],
    pub timeout_secs: Option<u64>,
    pub exec_command: Option<&'a str>,
    pub json: bool,
}

pub fn watch(ctx: &Ctx, reference: Option<&str>, args: WatchArgs) -> Result<i32> {
    let WatchArgs {
        tags,
        quorum,
        until,
        timeout_secs,
        exec_command,
        json,
    } = args;
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
            let value = watch_hook_payload(&selection, &hits, until);
            if json {
                println!("{}", serde_json::to_string(&value)?);
            } else {
                report_reached(&selection, &hits, until);
            }
            if let Some(command) = exec_command {
                let mut diagnostic = serde_json::to_vec(&value)?;
                diagnostic.push(b'\n');
                run_hook(command, &diagnostic, &value);
            }
            Ok(0)
        }
        Ok(None) => {
            report_timeout(until, json)?;
            Ok(2)
        }
        Err(_) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
            report_timeout(until, json)?;
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
/// A timeout is an outcome a script has to branch on, so it is reported in
/// whichever shape the caller asked for rather than only as prose.
fn report_timeout(until: &[WatchUntil], json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "event_type": "watch-timeout",
                "condition": until_labels(until),
            }))?
        );
    } else {
        println!("timeout: {}", until_labels(until));
    }
    Ok(())
}

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
                "event_id": hit.event_id,
            })).collect::<Vec<_>>(),
            "condition": until_labels(until),
            "event_id": "",
            "event_type": "watch-reached",
        }),
        _ => serde_json::json!({
            "change_id": hits[0].change_id,
            // Absent rather than empty when the condition is derived from
            // elapsed time or from policy: a field that always holds an event
            // ID should not sometimes hold a placeholder.
            "condition": hits[0].condition.label(),
            "event_id": hits[0].event_id,
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
                if let Some(event_id) = watch_reached(ctx, &store, change_id, *condition)? {
                    hits.push(WatchHit {
                        change_id: change_id.clone(),
                        condition: *condition,
                        event_id,
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

/// Whether a condition holds, and the event that made it hold when one did.
/// The outer `Option` is the answer; the inner one is whether an event can be
/// named for it.
fn watch_reached(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    until: WatchUntil,
) -> Result<Option<Option<String>>> {
    let events = store.load_events(change_id)?;
    let state = state::reduce(&events)?;
    let snapshot_event = || {
        events
            .iter()
            .rev()
            .find(|event| matches!(event.payload, Payload::PatchsetAdded { .. }))
            .map(|event| event.event_id.clone())
    };
    Ok(match until {
        WatchUntil::Snapshot => state.latest_patchset().is_some().then(snapshot_event),
        WatchUntil::Stalled => state
            .claim
            .as_ref()
            .is_some_and(|claim| state::claim_timing_at(claim, chrono::Utc::now()).stale)
            .then_some(None),
        WatchUntil::Ready => ctx.report(store, &state)?.integrate_ready.then_some(None),
        WatchUntil::Integrated => state
            .closure
            .as_ref()
            .filter(|closure| closure.outcome == Closure::Integrated)
            .map(|closure| Some(closure.event_id.clone())),
        WatchUntil::Closed => state
            .closure
            .as_ref()
            .map(|closure| Some(closure.event_id.clone())),
    })
}
