use super::*;

pub fn message(
    ctx: &Ctx,
    reference: &str,
    message_type: MessageType,
    summary: String,
    detail: Option<String>,
    json: Option<String>,
    severity: MessageSeverity,
) -> Result<()> {
    let summary = summary.trim();
    if summary.is_empty() {
        bail!("message summary must be a non-empty single line");
    }
    if summary.contains(['\n', '\r']) {
        bail!("message summary must be a single line");
    }
    let metadata = match json {
        None => None,
        Some(raw) => {
            let value: serde_json::Value =
                serde_json::from_str(&raw).context("--json must be valid JSON")?;
            if !value.is_object() {
                bail!("--json must be a JSON object");
            }
            Some(value)
        }
    };
    let detail = detail.and_then(|detail| {
        let trimmed = detail.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    });

    let store = ctx.store()?;
    let (change_id, _st) = ctx.load_state(&store, reference)?;
    let event = ctx.event(
        &store,
        &change_id,
        Payload::Message {
            message_type,
            severity,
            summary: summary.to_string(),
            detail,
            metadata,
        },
    );
    store.append_event(&event)?;
    println!("message: {} [{}]", message_type.as_str(), severity.as_str());
    println!("event: {}", event.event_id);
    Ok(())
}

pub fn messages(
    ctx: &Ctx,
    change: Option<&str>,
    message_type: Option<MessageType>,
    severity: Option<MessageSeverity>,
    since: Option<String>,
    json: bool,
) -> Result<()> {
    let store = ctx.store()?;
    let change_filter = change
        .map(|reference| store.resolve_change(reference))
        .transpose()?;
    let since = since
        .as_deref()
        .map(|raw| {
            chrono::DateTime::parse_from_rfc3339(raw)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .with_context(|| format!("invalid --since instant {raw:?}; expected ISO 8601"))
        })
        .transpose()?;

    let states = ctx.load_all_states(&store)?;
    let mut views: Vec<MessageView> = Vec::new();
    for state in states.values() {
        if change_filter
            .as_deref()
            .is_some_and(|id| id != state.change_id)
        {
            continue;
        }
        for message in &state.messages {
            if message_type.is_some_and(|wanted| wanted != message.message_type) {
                continue;
            }
            if severity.is_some_and(|wanted| wanted != message.severity) {
                continue;
            }
            if since.is_some_and(|floor| message.created_at < floor) {
                continue;
            }
            views.push(MessageView {
                change_id: &state.change_id,
                event_id: &message.event_id,
                event_type: "message",
                message_type: message.message_type,
                severity: message.severity,
                summary: &message.summary,
                detail: message.detail.as_deref(),
                metadata: message.metadata.as_ref(),
                actor: &message.actor,
                harness: message.harness.as_deref(),
                session: message.session.as_deref(),
                created_at: message.created_at,
            });
        }
    }
    // Newest first; event IDs are ULIDs, so they break created_at ties in
    // append order deterministically.
    views.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.event_id.cmp(a.event_id))
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&views)?);
    } else {
        for view in &views {
            println!(
                "{} [{}/{}] {} — {} ({})",
                view.change_id,
                view.message_type.as_str(),
                view.severity.as_str(),
                view.summary,
                view.actor,
                view.created_at.to_rfc3339()
            );
        }
    }
    Ok(())
}

/// Classify every open change into its queue buckets. Shared by `inbox` and
/// `catchup` so the queue has one derivation.
fn collect_inbox(
    ctx: &Ctx,
    store: &crate::store::Store,
    assigned_to: Option<&str>,
) -> Result<crate::inbox::Inbox> {
    let states = ctx.load_all_states(store)?;
    let filter = assigned_to.map(str::trim).filter(|f| !f.is_empty());
    let mut inbox = crate::inbox::Inbox::new(filter.map(str::to_string));
    for state in states.values() {
        if let Some(wanted) = filter {
            if state.assigned_to.as_deref() != Some(wanted) {
                continue;
            }
        }
        // An audit obligation is the one queue item that survives closure.
        inbox.absorb_audit_debt(state);
        if state.is_closed() {
            continue;
        }
        let report = ctx.report(store, state)?;
        inbox.absorb(state, &report);
    }
    inbox.sort_by_priority();
    Ok(inbox)
}

pub fn inbox(ctx: &Ctx, assigned_to: Option<String>, json: bool) -> Result<()> {
    let mut inbox = collect_inbox(ctx, &ctx.store()?, assigned_to.as_deref())?;
    inbox.journal = journal_backlog(ctx);

    if json {
        println!("{}", serde_json::to_string_pretty(&inbox)?);
    } else {
        for (name, rows) in inbox.sections() {
            println!("## {name}");
            if rows.is_empty() {
                println!("  (none)");
            }
            for row in rows {
                let claim_details = row
                    .owner
                    .as_ref()
                    .zip(row.stage.as_deref())
                    .zip(row.age_seconds)
                    .map(|((owner, stage), age_seconds)| {
                        format!(
                            " [owner: {}/{}/{}; stage: {stage}; age: {age_seconds}s]",
                            owner.actor, owner.harness, owner.session
                        )
                    })
                    .unwrap_or_default();
                println!(
                    "  {}  {} → {}{}{}",
                    row.change_id,
                    row.title,
                    row.next_actor,
                    claim_details,
                    row.assigned_to
                        .as_deref()
                        .map(|a| format!(" [assigned: {a}]"))
                        .unwrap_or_default()
                );
                // A held row that cannot name its holds cannot be acted on:
                // releasing one takes the event that set it.
                for hold in &row.holds {
                    println!(
                        "    hold {} by {}: {}",
                        hold.hold_event_id, hold.held_by, hold.reason
                    );
                }
            }
        }
        render_journal_backlog(inbox.journal.as_ref());
    }
    Ok(())
}

/// Outstanding review obligations, with the reason each was taken on.
///
/// `inbox` lists them as rows; here they carry their reasons, because the
/// question when picking one up is what review is owed, not merely that one is.
fn render_audit_debts(ctx: &Ctx, store: &crate::store::Store) -> Result<()> {
    let states = ctx.load_all_states(store)?;
    let mut owed: Vec<_> = states
        .values()
        .filter(|state| state.audit_debt_outstanding())
        .collect();
    if owed.is_empty() {
        return Ok(());
    }
    owed.sort_by(|a, b| a.change_id.cmp(&b.change_id));
    println!("audit-owed ({}):", owed.len());
    for state in owed {
        let reason = state
            .audit_debt
            .as_ref()
            .map(|debt| debt.reason.as_str())
            .unwrap_or_default();
        println!("  {}  {}", state.change_id, state.title);
        println!("    owed: {reason}");
    }
    println!("  discharge with: arc audit <change> --verdict <v>");
    Ok(())
}

/// The journal's actionable queue, summarized for the inbox. A journal that
/// cannot be resolved is not an error here: the inbox reports ledger state
/// either way.
pub(crate) fn journal_backlog(ctx: &Ctx) -> Option<crate::inbox::JournalBacklog> {
    let items = crate::journal::collect_open(ctx, None).ok()?;
    let (open, later, feature_requests) = items.tier_counts();
    Some(crate::inbox::JournalBacklog {
        dir: items.dir().to_string(),
        open,
        later,
        feature_requests,
        preview: items
            .primary_preview(3)
            .into_iter()
            .map(|(file, kind, heading)| crate::inbox::JournalBacklogRow {
                file,
                kind,
                heading,
            })
            .collect(),
    })
}

pub(crate) fn render_journal_backlog(backlog: Option<&crate::inbox::JournalBacklog>) {
    let Some(backlog) = backlog else {
        return;
    };
    println!("## journal backlog");
    if backlog.open == 0 && backlog.later == 0 && backlog.feature_requests == 0 {
        println!("  (none)  {}", backlog.dir);
        return;
    }
    println!(
        "  {} open, {} later, {} feature-request  {}",
        backlog.open, backlog.later, backlog.feature_requests, backlog.dir
    );
    for row in &backlog.preview {
        println!("  {}  {}  {}", row.file, row.kind, row.heading);
    }
    println!("  full queue: arc journal open");
}

/// Orient a session in one call: the ledger's actionable buckets, then the
/// journal's lanes, memories, and backlog. `inbox` answers what is already
/// open; this answers what is waiting, which is the larger question and the
/// one a session starting cold actually has.
pub fn catchup(ctx: &Ctx, limit: usize, json: bool) -> Result<i32> {
    let store = ctx.store()?;
    let inbox = collect_inbox(ctx, &store, None)?;
    let journal = crate::journal::orientation(ctx);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": "arc-catchup/1",
                "ledger": inbox,
                "journal": journal.as_ref().ok(),
            }))?
        );
        return Ok(0);
    }

    println!("ledger: {}", store.root.display());
    let mut any = false;
    for (name, rows) in inbox.sections() {
        // Owed audits are rendered below with their reasons, which is the
        // detail that matters when picking one up.
        if rows.is_empty() || name == "audit-owed" {
            continue;
        }
        any = true;
        println!("{name} ({}):", rows.len());
        for row in rows.iter().take(limit) {
            println!("  {}  {} → {}", row.change_id, row.title, row.next_actor);
        }
    }
    if !any {
        println!("  no open changes");
    }
    render_audit_debts(ctx, &store)?;
    match journal {
        Ok(journal) => journal.render(),
        Err(error) => println!("journal: unavailable ({error:#})"),
    }
    Ok(0)
}
