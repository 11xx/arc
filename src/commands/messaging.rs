use super::*;
use chrono::{DateTime, Utc};

#[derive(Debug)]
struct DebtEntry {
    change_id: String,
    title: String,
    reason: String,
    declared_at: DateTime<Utc>,
    surfaces: Vec<String>,
}

impl DebtEntry {
    fn surface_detail(&self) -> String {
        if self.surfaces.is_empty() {
            "unknown".to_string()
        } else {
            self.surfaces.join(", ")
        }
    }

    fn detail(&self) -> String {
        format!(
            "{} ({}): owed: {}; surfaces: {}",
            self.change_id,
            crate::render::one_line(&self.title),
            crate::render::one_line(&self.reason),
            self.surface_detail()
        )
    }
}

#[derive(Debug)]
pub(crate) struct DebtSummary {
    entries: Vec<DebtEntry>,
    oldest_age_seconds: u64,
    surfaces: Vec<String>,
    priority_advisory: bool,
}

impl DebtSummary {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn detail(&self) -> String {
        let surfaces = if self.surfaces.is_empty() {
            "unknown".to_string()
        } else {
            self.surfaces.join(", ")
        };
        format!(
            "{} outstanding; oldest {}; surfaces ({}): {}{}",
            self.entries.len(),
            crate::journal::format_age(self.oldest_age_seconds),
            self.surfaces.len(),
            surfaces,
            if self.priority_advisory {
                "; priority: advisory"
            } else {
                ""
            }
        )
    }

    pub(crate) fn render_summary(&self) {
        if !self.is_empty() {
            println!(
                "debt-owed ({}): {}; discharge with: arc audit <change> --verdict <v>",
                self.entries.len(),
                self.detail()
            );
        }
    }

    fn touching<'a>(&'a self, ctx: &Ctx, state: &ChangeState) -> Vec<&'a DebtEntry> {
        let changed = change_surfaces(ctx, state);
        if changed.is_empty() {
            return Vec::new();
        }
        self.entries
            .iter()
            .filter(|entry| {
                entry
                    .surfaces
                    .iter()
                    .any(|surface| changed.contains(surface))
            })
            .collect()
    }

    pub(crate) fn render_touched(&self, ctx: &Ctx, state: &ChangeState) {
        for entry in self.touching(ctx, state) {
            println!("    debt {}", entry.detail());
        }
    }

    pub(crate) fn advisories_for(
        &self,
        ctx: &Ctx,
        state: &ChangeState,
    ) -> Vec<crate::status::Advisory> {
        if self.is_empty() {
            return Vec::new();
        }
        let mut advisories = vec![crate::status::Advisory {
            code: "debt-summary",
            detail: self.detail(),
        }];
        for entry in self.touching(ctx, state) {
            advisories.push(crate::status::Advisory {
                code: "debt-touched",
                detail: entry.detail(),
            });
        }
        advisories
    }
}

pub(crate) fn collect_debts(
    ctx: &Ctx,
    states: &BTreeMap<String, ChangeState>,
) -> Result<DebtSummary> {
    let mut entries = Vec::new();
    for state in states.values() {
        let Some(debt) = state.debt.as_ref().filter(|_| state.debt_outstanding()) else {
            continue;
        };
        entries.push(DebtEntry {
            change_id: state.change_id.clone(),
            title: state.title.clone(),
            reason: debt.reason.clone(),
            declared_at: debt.declared_at,
            surfaces: debt_surfaces(ctx, state, debt),
        });
    }
    entries.sort_by(|a, b| a.change_id.cmp(&b.change_id));
    let oldest = entries
        .iter()
        .map(|entry| entry.declared_at)
        .min()
        .unwrap_or_else(Utc::now);
    let oldest_age_seconds = Utc::now()
        .signed_duration_since(oldest)
        .num_seconds()
        .max(0) as u64;
    let mut surfaces = BTreeSet::new();
    for entry in &entries {
        surfaces.extend(entry.surfaces.iter().cloned());
    }
    let priority_advisory = if entries.is_empty() {
        false
    } else {
        let policy = crate::policy::load(&crate::gitio::toplevel(&ctx.cwd)?)?;
        policy
            .policy
            .debt_count_threshold
            .is_some_and(|threshold| entries.len() > threshold)
            || policy
                .policy
                .debt_age_threshold_seconds
                .is_some_and(|threshold| oldest_age_seconds > threshold)
    };
    Ok(DebtSummary {
        entries,
        oldest_age_seconds,
        surfaces: surfaces.into_iter().collect(),
        priority_advisory,
    })
}

fn debt_surfaces(ctx: &Ctx, state: &ChangeState, debt: &crate::state::Debt) -> Vec<String> {
    let range = state
        .closure
        .as_ref()
        .and_then(|closure| {
            closure
                .target_before
                .as_deref()
                .zip(closure.integrated_commit.as_deref())
        })
        .or_else(|| {
            debt.patchset_id.as_deref().and_then(|patchset_id| {
                state
                    .patchsets
                    .iter()
                    .find(|patchset| patchset.id == patchset_id)
                    .map(|patchset| (patchset.base.as_str(), patchset.head.as_str()))
            })
        })
        .or_else(|| {
            state
                .latest_patchset()
                .map(|patchset| (patchset.base.as_str(), patchset.head.as_str()))
        });
    range
        .and_then(|(base, head)| crate::gitio::changed_paths(&ctx.cwd, base, head).ok())
        .unwrap_or_default()
}

fn change_surfaces(ctx: &Ctx, state: &ChangeState) -> BTreeSet<String> {
    let Some(head) = crate::gitio::branch_head(&ctx.cwd, &state.branch)
        .ok()
        .or_else(|| {
            state
                .latest_patchset()
                .map(|patchset| patchset.head.clone())
        })
    else {
        return BTreeSet::new();
    };
    crate::gitio::changed_paths(&ctx.cwd, &state.base, &head)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

#[derive(Debug)]
struct PassMembers {
    members: BTreeSet<String>,
    ended: bool,
}

#[derive(Debug)]
pub(crate) struct ReviewQueue {
    covered: BTreeMap<String, Vec<String>>,
    uncovered: Vec<String>,
}

impl ReviewQueue {
    pub(crate) fn is_empty(&self) -> bool {
        self.covered.is_empty() && self.uncovered.is_empty()
    }

    fn pass_count(&self) -> usize {
        self.covered.len() + usize::from(!self.uncovered.is_empty())
    }

    pub(crate) fn detail(&self) -> String {
        let changes = self.covered.values().map(Vec::len).sum::<usize>() + self.uncovered.len();
        let passes = self.pass_count();
        let pass_word = if passes == 1 { "pass" } else { "passes" };
        let mut groups = self
            .covered
            .iter()
            .map(|(pass, members)| format!("pass {pass} covers {} changes", members.len()))
            .collect::<Vec<_>>();
        if !self.uncovered.is_empty() {
            groups.push(format!(
                "{} changes coverable by one pass",
                self.uncovered.len()
            ));
        }
        format!(
            "review queue: {changes} changes, {passes} {pass_word}; {}",
            groups.join("; ")
        )
    }

    pub(crate) fn render(&self) {
        if self.is_empty() {
            return;
        }
        println!("{}", self.detail());
        for (pass, members) in &self.covered {
            println!("  pass {pass} ({} changes):", members.len());
            for change_id in members {
                println!("    {change_id}");
            }
        }
        if !self.uncovered.is_empty() {
            println!(
                "  coverable by one pass ({} changes):",
                self.uncovered.len()
            );
            for change_id in &self.uncovered {
                println!("    {change_id}");
            }
        }
    }
}

pub(crate) fn collect_review_queue(
    store: &crate::store::Store,
    states: &BTreeMap<String, ChangeState>,
) -> Result<ReviewQueue> {
    let passes = open_review_passes(store)?;
    let mut covered = BTreeMap::<String, Vec<String>>::new();
    let mut uncovered = Vec::new();
    for state in states.values().filter(|state| {
        state.closure.is_none()
            && crate::inbox::needs_review(state)
            && state.latest_patchset().is_some()
    }) {
        let patchset = state.latest_patchset().expect("filtered above");
        let member = format!("{}:{}", state.change_id, patchset.id);
        if let Some(pass_id) = passes
            .iter()
            .find(|(_, members)| members.contains(&member))
            .map(|(pass_id, _)| pass_id)
        {
            covered
                .entry(pass_id.clone())
                .or_default()
                .push(state.change_id.clone());
        } else {
            uncovered.push(state.change_id.clone());
        }
    }
    Ok(ReviewQueue { covered, uncovered })
}

fn open_review_passes(store: &crate::store::Store) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut passes = BTreeMap::<String, PassMembers>::new();
    for event in store.load_repository_events()? {
        match event.payload {
            Payload::ReviewPassOpened {
                pass_id, members, ..
            } => {
                crate::ids::validate_id_component(&pass_id).with_context(|| {
                    format!(
                        "review pass event {} has an invalid pass id",
                        event.event_id
                    )
                })?;
                if members.is_empty() {
                    bail!("review pass {pass_id} has no members");
                }
                let mut unique = BTreeSet::new();
                for member in members {
                    validate_pass_member(&member).with_context(|| {
                        format!("review pass {pass_id} has invalid member {member:?}")
                    })?;
                    if !unique.insert(member.clone()) {
                        bail!("review pass {pass_id} repeats member {member}");
                    }
                }
                if passes
                    .insert(
                        pass_id.clone(),
                        PassMembers {
                            members: unique,
                            ended: false,
                        },
                    )
                    .is_some()
                {
                    bail!("review pass {pass_id} was opened more than once");
                }
            }
            Payload::ReviewPassCompleted { pass_id, .. }
            | Payload::ReviewPassAbandoned { pass_id, .. } => {
                let pass = passes.get_mut(&pass_id).with_context(|| {
                    format!("review pass ending event references unknown pass {pass_id:?}")
                })?;
                if pass.ended {
                    bail!("review pass {pass_id} has more than one ending");
                }
                pass.ended = true;
            }
            _ => {}
        }
    }
    Ok(passes
        .into_iter()
        .filter_map(|(pass_id, pass)| (!pass.ended).then_some((pass_id, pass.members)))
        .collect())
}

fn validate_pass_member(member: &str) -> Result<()> {
    if member.trim() != member {
        bail!("member has surrounding whitespace");
    }
    let (change_id, patchset_id) = member
        .split_once(':')
        .context("expected one ':' between change and patchset")?;
    if patchset_id.contains(':') {
        bail!("member contains more than one ':'");
    }
    crate::ids::validate_id_component(change_id)?;
    crate::ids::validate_id_component(patchset_id)?;
    Ok(())
}

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
        inbox.absorb_debt(state);
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

/// Outstanding review obligations as one actionable summary row.
fn render_debts(summary: &DebtSummary) {
    summary.render_summary();
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

/// What the open changes' worktrees occupy, measured read-only. Build
/// output is reproducible, so this is cost, not content — the fact a session
/// of parallel changes fills a filesystem without any command reporting it.
/// Forks this repository holds, from the journal's `fork-<slug>` markers
/// plus any `fork/*` branch. They are listed because `catchup` answers what
/// is waiting: a fork is work in progress by intent, and the operator's
/// next session should know it exists without finding the worktree by hand.
/// Arc does not gate forks, so this is orientation, not obligation.
fn render_forks(ctx: &Ctx) {
    if let Ok(forks) = crate::commands::fork::list_entries(ctx) {
        let open: Vec<_> = forks.iter().filter(|fork| fork.retired.is_none()).collect();
        if !open.is_empty() {
            println!("forks ({}):", open.len());
            for fork in &open {
                let place = fork
                    .worktree
                    .as_deref()
                    .map(|path| format!(" ({path})"))
                    .unwrap_or_default();
                println!(
                    "  {}  {}  +{} over {}{}",
                    fork.slug, fork.branch, fork.ahead, fork.base_branch, place
                );
            }
        }
    }
}

fn render_worktree_accounting(accounting: &crate::worktree_usage::WorktreeAccounting) {
    if accounting.is_empty() {
        return;
    }
    match accounting.total_bytes {
        Some(total) => println!(
            "worktrees: {} across {} open worktree(s)",
            crate::worktree_usage::human(total),
            accounting.changes.len()
        ),
        None => println!(
            "worktrees: {} open, size unavailable",
            accounting.changes.len()
        ),
    }
    for usage in &accounting.changes {
        let size = usage
            .bytes
            .map(crate::worktree_usage::human)
            .unwrap_or_else(|| "size unknown".to_string());
        println!("  {}  {}  {}", usage.change_id, size, usage.path);
    }
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

    let states = ctx.load_all_states(&store)?;
    let debts = collect_debts(ctx, &states)?;
    let review_queue = collect_review_queue(&store, &states)?;
    let worktrees = crate::worktree_usage::measure(&ctx.cwd, &states);
    println!("ledger: {}", store.root.display());
    let mut any = false;
    let mut rendered_debt_details = BTreeSet::new();
    for (name, rows) in inbox.sections() {
        // Owed audits are rendered below as one summary, while touched debts
        // are attached to the change whose diff can carry them forward.
        if rows.is_empty() || name == "debt-owed" {
            continue;
        }
        any = true;
        if name == "needs-review" {
            review_queue.render();
        }
        println!("{name} ({}):", rows.len());
        for row in rows.iter().take(limit) {
            println!("  {}  {} → {}", row.change_id, row.title, row.next_actor);
            if rendered_debt_details.insert(row.change_id.clone()) {
                if let Some(state) = states.get(&row.change_id) {
                    debts.render_touched(ctx, state);
                }
            }
        }
    }
    if !any {
        println!("  no open changes");
    }
    render_forks(ctx);
    render_worktree_accounting(&worktrees);
    render_debts(&debts);
    match journal {
        Ok(journal) => journal.render(),
        Err(error) => println!("journal: unavailable ({error:#})"),
    }
    Ok(0)
}
