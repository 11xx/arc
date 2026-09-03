//! Raw event replay/follow with resumable cursors and shell hooks, plus
//! polling watches that can wait for the first of several conditions.

use super::*;

/// Replay raw ledger events as compact NDJSON, optionally continuing as new
/// event files arrive. Event IDs are ULIDs, so replay and each observed polling
/// batch can be sorted across changes; concurrent appends may cross batches.
/// What to stream, as the CLI expresses it. One struct rather than eight
/// arguments, because the selectors are mutually exclusive and a call site
/// should show which one it chose.
pub struct EventsArgs<'a> {
    pub follow: bool,
    pub change: Option<&'a str>,
    pub tags: &'a [String],
    pub repository_scope: bool,
    pub event_type: Option<&'a str>,
    pub since: Option<ulid::Ulid>,
    pub exec_command: Option<&'a str>,
}

pub fn events(ctx: &Ctx, args: EventsArgs<'_>) -> Result<()> {
    let EventsArgs {
        follow,
        change,
        tags,
        repository_scope,
        event_type,
        since,
        exec_command,
    } = args;
    if change.is_some() && !tags.is_empty() {
        bail!("--change and --tag select different scopes; supply one");
    }
    let store = ctx.store()?;
    // A flag rather than a reserved reference value: `repository` is a
    // perfectly good slug, and a magic string would shadow the change a
    // caller actually named.
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

    // Membership is what it is when the stream is read, but deriving it means
    // replaying every change, so it is re-derived only when an event that can
    // change it has appeared. A steady stream of ordinary events costs
    // nothing.
    let mut tagged: Option<BTreeSet<String>> = if tags.is_empty() {
        None
    } else {
        Some(resolve_tagged(ctx, &tags)?.into_iter().collect())
    };
    loop {
        let raw_events = match (&change_id, repository_scope) {
            (Some(id), _) => store.raw_events_unseen(id, &seen)?,
            (None, true) => store.raw_repository_events_unseen(&seen)?,
            (None, false) => store.raw_events_all_unseen(&seen)?,
        };
        let observed_events = !raw_events.is_empty();
        if tagged.is_some()
            && raw_events.iter().any(|(_, value)| {
                matches!(
                    value.get("event_type").and_then(serde_json::Value::as_str),
                    Some("change-opened") | Some("metadata-updated")
                )
            })
        {
            tagged = Some(resolve_tagged(ctx, &tags)?.into_iter().collect());
        }
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
    provisional: Option<String>,
}

/// One condition that holds, with the event that made it hold when one can be
/// named. The provisional reason travels with an approving verdict so the
/// watch diagnostic does not have to replay the selection a second time.
struct WatchReached {
    event_id: Option<String>,
    provisional: Option<String>,
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
    if let Some(file) =
        reference.and_then(|reference| crate::journal::artifact_subject(&ctx.cwd, reference))
    {
        return watch_artifact(
            ctx,
            &file,
            tags,
            quorum,
            until,
            timeout_secs,
            exec_command,
            json,
        );
    }
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

/// Wait until the claim on a journal artifact stops being worked.
///
/// Only `stalled` is a question about an artifact. The rest of the vocabulary
/// asks about patchsets, verdicts, and gates, which belong to a change; a
/// watch that silently never fired would be indistinguishable from work still
/// in progress, which is the failure the whole surface exists to avoid.
#[allow(clippy::too_many_arguments)]
fn watch_artifact(
    ctx: &Ctx,
    file: &str,
    tags: &[String],
    quorum: Option<WatchQuorum>,
    until: &[WatchUntil],
    timeout_secs: Option<u64>,
    exec_command: Option<&str>,
    json: bool,
) -> Result<i32> {
    if !tags.is_empty() || quorum.is_some() {
        bail!("--tag, --any, and --all select changes, not a journal artifact");
    }
    if let Some(other) = until
        .iter()
        .find(|condition| !matches!(condition, WatchUntil::Stalled))
    {
        bail!(
            "--until {} asks about a change; a journal artifact answers only `stalled`",
            other.label()
        );
    }
    if until.is_empty() {
        bail!("watch requires --until");
    }
    let deadline = timeout_secs.map(|timeout| Instant::now() + Duration::from_secs(timeout));
    let mut poll_interval = POLL_MIN;
    let claim_id = loop {
        if let Some(claim_id) = crate::journal::stalled_artifact_claim(ctx, file)? {
            break Some(claim_id);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break None;
        }
        let sleep_for = deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .map_or(poll_interval, |remaining| poll_interval.min(remaining));
        thread::sleep(sleep_for);
        poll_interval = (poll_interval * 2).min(POLL_MAX);
    };
    let Some(claim_id) = claim_id else {
        report_timeout(until, json)?;
        return Ok(2);
    };
    let value = serde_json::json!({
        "event_type": "watch-reached",
        "condition": WatchUntil::Stalled.label(),
        "file": file,
        "claim_id": claim_id,
    });
    if json {
        println!("{}", serde_json::to_string(&value)?);
    } else {
        println!("reached: {} ({file})", WatchUntil::Stalled.label());
    }
    if let Some(command) = exec_command {
        let mut diagnostic = serde_json::to_vec(&value)?;
        diagnostic.push(b'\n');
        run_hook(command, &diagnostic, &value);
    }
    Ok(0)
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
        WatchSelection::Single(_) => println!(
            "reached: {}{}",
            hits[0].condition.label(),
            provisional_suffix(&hits[0].provisional)
        ),
        WatchSelection::Tagged(_, WatchQuorum::Any) => {
            println!(
                "reached: {} ({}){}",
                hits[0].condition.label(),
                hits[0].change_id,
                provisional_suffix(&hits[0].provisional)
            )
        }
        WatchSelection::Tagged(change_ids, WatchQuorum::All) => {
            let reasons = hits
                .iter()
                .filter_map(|hit| hit.provisional.as_deref().map(render::one_line))
                .collect::<Vec<_>>();
            if reasons.is_empty() {
                println!(
                    "reached: {} ({} changes)",
                    until_labels(until),
                    change_ids.len()
                );
            } else {
                println!(
                    "reached: {} ({} changes; provisional: {})",
                    until_labels(until),
                    change_ids.len(),
                    reasons.join(", ")
                );
            }
        }
    }
}

fn provisional_suffix(reason: &Option<String>) -> String {
    reason
        .as_deref()
        .map(|reason| format!(" (provisional: {})", render::one_line(reason)))
        .unwrap_or_default()
}

fn watch_hook_payload(
    selection: &WatchSelection,
    hits: &[WatchHit],
    until: &[WatchUntil],
) -> serde_json::Value {
    match selection {
        // Each member carries its own change, condition, and satisfying
        // event, so there is nothing for a top-level placeholder to say.
        WatchSelection::Tagged(_, WatchQuorum::All) => serde_json::json!({
            "changes": hits.iter().map(watch_hit_object).collect::<Vec<_>>(),
            "condition": until_labels(until),
            "event_type": "watch-reached",
        }),
        _ => {
            let mut value = watch_hit_object(&hits[0]);
            value["event_type"] = "watch-reached".into();
            value
        }
    }
}

/// One satisfied member. `event_id` is present only when an event satisfied
/// the condition: a field that otherwise holds an event ID should not
/// sometimes hold a placeholder, and a condition derived from elapsed time or
/// from policy was satisfied by no event at all.
fn watch_hit_object(hit: &WatchHit) -> serde_json::Value {
    let mut value = serde_json::json!({
        "change_id": hit.change_id,
        "condition": hit.condition.label(),
    });
    if let Some(event_id) = &hit.event_id {
        value["event_id"] = event_id.clone().into();
    }
    if let Some(reason) = &hit.provisional {
        value["provisional"] = reason.clone().into();
    }
    value
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
                if let Some(reached) = watch_reached(ctx, &store, change_id, *condition)? {
                    hits.push(WatchHit {
                        change_id: change_id.clone(),
                        condition: *condition,
                        event_id: reached.event_id,
                        provisional: reached.provisional,
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
/// The outer `Option` is the answer; `event_id` is whether an event can be
/// named for it.
fn watch_reached(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    until: WatchUntil,
) -> Result<Option<WatchReached>> {
    let events = store.load_events(change_id)?;
    let state = state::reduce_following(&events, &store.rewrites())?;
    let snapshot_event = || {
        events
            .iter()
            .rev()
            .find(|event| matches!(event.payload, Payload::PatchsetAdded { .. }))
            .map(|event| event.event_id.clone())
    };
    Ok(match until {
        WatchUntil::Snapshot => state.latest_patchset().is_some().then(|| WatchReached {
            event_id: snapshot_event(),
            provisional: None,
        }),
        WatchUntil::Stalled => state
            .claim
            .as_ref()
            .is_some_and(|claim| state::claim_timing_at(claim, chrono::Utc::now()).stale)
            .then_some(WatchReached {
                event_id: None,
                provisional: None,
            }),
        // Any verdict against the patchset under review, whatever it concluded.
        // `ready` cannot express this: a review returning changes-requested or
        // comment-only never satisfies it, so the watch runs to its timeout and
        // leaves *still working*, *changes requested*, and *reviewer died*
        // indistinguishable from each other. The caller reads the verdict named
        // here to learn which.
        //
        // Bound to the latest patchset, because a verdict on an earlier one was
        // answered by a commit since: satisfying the wait with it would report a
        // review of code the reviewer never saw.
        WatchUntil::Reviewed => state.latest_patchset().and_then(|latest| {
            state
                .verdicts
                .iter()
                .rev()
                .find(|verdict| verdict.patchset_id == latest.id)
                .map(|verdict| WatchReached {
                    event_id: Some(verdict.event_id.clone()),
                    provisional: None,
                })
        }),
        // The sole authoritative verdict, which is the same question the
        // approval gate asks. Recency is not that question: a corroborating
        // approval is never a tip, and a contested graph has no authority at
        // all, so a watch reading the newest event would report reached on
        // exactly the changes `check` refuses.
        //
        // A provisional approval gates checks and integration, so it satisfies
        // this wait; its reason is carried into the diagnostic for the caller.
        WatchUntil::Approved => {
            // Authority is necessary and not sufficient: an approval the
            // repository's policy refuses — a self-approval where one is
            // forbidden, or one resting on an identity arc assumed — is a
            // verdict `check` will not integrate on. The status layer already
            // decides that, so read its answer instead of asking a narrower
            // question here and reporting reached on a change that cannot
            // merge.
            let report = ctx.report(store, &state)?;
            report
                .verdict
                .as_ref()
                .filter(|verdict| verdict.valid_for_current_head)
                .filter(|verdict| verdict.verdict == Verdict::Approved)
                .and_then(|verdict| {
                    state
                        .latest_verdict()
                        .filter(|authoritative| authoritative.patchset_id == verdict.patchset_id)
                        .map(|authoritative| WatchReached {
                            event_id: Some(authoritative.event_id.clone()),
                            provisional: authoritative.provisional.clone(),
                        })
                })
        }
        WatchUntil::GatesGreen => ctx
            .report(store, &state)?
            .gates
            .iter()
            // No required gates is already the universal predicate: no
            // required gate is ungreen, so the condition is satisfied.
            .all(|gate| gate.green_at_head)
            .then_some(WatchReached {
                event_id: None,
                provisional: None,
            }),
        WatchUntil::Ready => ctx
            .report(store, &state)?
            .integrate_ready
            .then_some(WatchReached {
                event_id: None,
                provisional: None,
            }),
        WatchUntil::Blocked => {
            state
                .blocked_on_stages
                .last()
                .cloned()
                .map(|event_id| WatchReached {
                    event_id: Some(event_id),
                    provisional: None,
                })
        }
        WatchUntil::BriefRecorded => state.latest_brief().map(|brief| WatchReached {
            event_id: Some(brief.event_id.clone()),
            provisional: None,
        }),
        WatchUntil::Integrated => state
            .closure
            .as_ref()
            .filter(|closure| closure.outcome == Closure::Integrated)
            .map(|closure| WatchReached {
                event_id: Some(closure.event_id.clone()),
                provisional: None,
            }),
        WatchUntil::Closed => state.closure.as_ref().map(|closure| WatchReached {
            event_id: Some(closure.event_id.clone()),
            provisional: None,
        }),
    })
}
