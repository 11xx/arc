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

pub fn inbox(ctx: &Ctx, assigned_to: Option<String>, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let states = ctx.load_all_states(&store)?;
    let filter = assigned_to
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty());
    let mut inbox = crate::inbox::Inbox::new(filter.map(str::to_string));
    for state in states.values() {
        if state.is_closed() {
            continue;
        }
        if let Some(wanted) = filter {
            if state.assigned_to.as_deref() != Some(wanted) {
                continue;
            }
        }
        let report = ctx.report(&store, state)?;
        inbox.absorb(state, &report);
    }
    inbox.sort_by_priority();

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
            }
        }
    }
    Ok(())
}
