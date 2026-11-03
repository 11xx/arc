//! Checkpoints: a named revision somebody is testing, with the changes and
//! journal todos open when it was named, read back against what has happened
//! since.

use crate::gitio::{self, Ancestry};
use crate::journal::{self, TodoRecord};
use crate::model::{CheckpointChange, CheckpointTodo, Closure, Event, Payload};
use crate::render::short_sha;
use crate::state::ChangeState;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, SubsecRound, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::Ctx;

const TAG_PREFIX: &str = "checkpoint/";

/// One recorded checkpoint, read off its event.
pub(crate) struct Recorded {
    pub(crate) event: Event,
    pub(crate) name: String,
    pub(crate) revision: String,
    pub(crate) tree: String,
    pub(crate) branch: Option<String>,
    pub(crate) tag: Option<String>,
    pub(crate) changes: Vec<CheckpointChange>,
    pub(crate) todos: Vec<CheckpointTodo>,
    pub(crate) filed_todos: Option<BTreeSet<String>>,
    pub(crate) note: Option<String>,
}

fn recorded(events: &[Event]) -> Vec<Recorded> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::CheckpointRecorded {
                name,
                revision,
                tree,
                branch,
                tag,
                changes,
                todos,
                filed_todos,
                note,
            } => Some(Recorded {
                event: event.clone(),
                name: name.clone(),
                revision: revision.clone(),
                tree: tree.clone(),
                branch: branch.clone(),
                tag: tag.clone(),
                changes: changes.clone(),
                todos: todos.clone(),
                filed_todos: filed_todos
                    .as_ref()
                    .map(|files| files.iter().cloned().collect()),
                note: note.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// The one checkpoint recorded under `name`. Two under one name, which only
/// an import can produce, refuse rather than answer for either.
pub(crate) fn find(store: &Store, name: &str) -> Result<Recorded> {
    let mut matches: Vec<Recorded> = recorded(&store.load_repository_events()?)
        .into_iter()
        .filter(|checkpoint| checkpoint.name == name)
        .collect();
    match matches.len() {
        0 => bail!("no checkpoint named {name:?}; `arc checkpoint list` names every one"),
        1 => Ok(matches.remove(0)),
        _ => bail!(
            "checkpoint {name:?} is recorded more than once (events {}); none is read",
            matches
                .iter()
                .map(|checkpoint| checkpoint.event.event_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A name that is also a valid tag component: ASCII letters, digits, `.`,
/// `_` and `-`, starting with a letter or digit.
fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.contains("..")
        && !name.ends_with('.')
        && !name.ends_with(".lock");
    if !valid {
        bail!(
            "checkpoint name {name:?} must start with a letter or digit and hold only \
             [A-Za-z0-9._-], without `..` or a trailing `.` or `.lock`"
        );
    }
    Ok(())
}

pub fn record_checkpoint(
    ctx: &Ctx,
    name: String,
    rev: Option<String>,
    tag: bool,
    note: Option<String>,
) -> Result<()> {
    validate_name(&name)?;
    let note = match note {
        Some(note) if note.trim().is_empty() => bail!("--note must not be blank"),
        note => note,
    };
    let store = ctx.store()?;
    let _repository_events = store.lock_repository_events()?;
    if let Some(existing) = recorded(&store.load_repository_events()?)
        .into_iter()
        .find(|checkpoint| checkpoint.name == name)
    {
        bail!(
            "checkpoint {name:?} is already recorded at {} (event {}); nothing was recorded",
            existing.revision,
            existing.event.event_id
        );
    }
    let cwd = ctx.cwd.as_path();
    let spelled = rev.as_deref().unwrap_or("HEAD");
    let revision = gitio::rev_parse(cwd, spelled)
        .with_context(|| format!("{spelled:?} does not name a commit"))?;
    let tree = gitio::commit_tree(cwd, &revision)?;
    let branch = recorded_branch(cwd, rev.as_deref(), &revision)?;
    let tag_name = tag.then(|| format!("{TAG_PREFIX}{name}"));
    if let Some(tag_name) = &tag_name {
        if gitio::ref_value(cwd, &format!("refs/tags/{tag_name}"))?.is_some() {
            bail!("tag {tag_name} already exists; nothing was recorded");
        }
    }

    let read = store.read()?;
    let mut changes = Vec::new();
    for change_id in read.change_ids()? {
        let state = read
            .state(&change_id)
            .with_context(|| format!("cannot read change {change_id}"))?;
        if state.is_closed() {
            continue;
        }
        let patchset = state.latest_patchset();
        changes.push(CheckpointChange {
            change_id: state.change_id.clone(),
            slug: state.slug.clone(),
            state: journal::stage_label(&state),
            patchset_id: patchset.map(|patchset| patchset.id.clone()),
            head: patchset.map(|patchset| patchset.head.clone()),
        });
    }
    let records = journal::todo_records(cwd)?;
    let filed_todos: Vec<String> = records.iter().map(|todo| todo.file.clone()).collect();
    let todos: Vec<CheckpointTodo> = records
        .into_iter()
        .filter(TodoRecord::is_open)
        .map(|todo| CheckpointTodo {
            file: todo.file,
            title: todo.title,
            item_key: todo.item_key,
            created_at: todo.created_at,
        })
        .collect();
    let (change_count, todo_count) = (changes.len(), todos.len());
    let mut event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::CheckpointRecorded {
            name: name.clone(),
            revision: revision.clone(),
            tree: tree.clone(),
            branch: branch.clone(),
            tag: tag_name.clone(),
            changes,
            todos,
            filed_todos: Some(filed_todos),
            note: note.clone(),
        },
    )?;

    let tag_object = match &tag_name {
        Some(tag_name) => Some(write_tag(cwd, tag_name, &name, &revision, note.as_deref())?),
        None => None,
    };
    if let Err(error) = store.append_local_repository_event(&mut event) {
        let tag = tag_name.as_deref().zip(tag_object.as_deref());
        return Err(failed_append(&store, cwd, &event, tag, error));
    }
    println!("checkpoint: {name}");
    println!("revision: {revision}");
    println!("tree: {tree}");
    if let Some(branch) = &branch {
        println!("branch: {branch}");
    }
    if let Some(tag_name) = &tag_name {
        println!("tag: {tag_name}");
    }
    println!("open changes: {change_count}");
    println!("open todos: {todo_count}");
    println!("event: {}", event.event_id);
    Ok(())
}

/// The error for a checkpoint append that failed, after settling the tag
/// written for it. The tag is deleted only when the event is known absent;
/// a published event, or one whose publication cannot be told, keeps it.
fn failed_append(
    store: &Store,
    cwd: &Path,
    event: &Event,
    tag: Option<(&str, &str)>,
    error: anyhow::Error,
) -> anyhow::Error {
    let kept = tag
        .map(|(tag_name, _)| format!("; tag {tag_name} is kept"))
        .unwrap_or_default();
    match store.repository_event_is_published(event) {
        Ok(true) => error.context(format!(
            "checkpoint event {} is published but not confirmed durable{kept}",
            event.event_id
        )),
        Err(inspection) => error.context(format!(
            "cannot tell whether checkpoint event {} was published ({inspection:#}){kept}",
            event.event_id
        )),
        Ok(false) => {
            let Some((tag_name, object)) = tag else {
                return error.context("nothing was recorded");
            };
            let reference = format!("refs/tags/{tag_name}");
            match gitio::git(cwd, &["update-ref", "-d", &reference, object]) {
                Ok(_) => error.context(format!(
                    "nothing was recorded; tag {tag_name} was deleted again"
                )),
                Err(cleanup) => error.context(format!(
                    "nothing was recorded, and tag {tag_name} could not be deleted: {cleanup:#}"
                )),
            }
        }
    }
}

/// The branch a checkpoint is on: the one `--rev` names, or the one `HEAD`
/// has checked out when the revision is `HEAD`'s commit.
fn recorded_branch(cwd: &Path, rev: Option<&str>, revision: &str) -> Result<Option<String>> {
    if let Some(rev) = rev {
        let named = rev.strip_prefix("refs/heads/").unwrap_or(rev);
        if gitio::ref_value(cwd, &format!("refs/heads/{named}"))?.is_some() {
            return Ok(Some(named.to_string()));
        }
    }
    if gitio::head_if_present(cwd)?.as_deref() == Some(revision) {
        return gitio::current_branch(cwd);
    }
    Ok(None)
}

/// Write the annotated tag with `git tag -a`, so the user's signing
/// configuration applies, and return the tag object it wrote.
fn write_tag(
    cwd: &Path,
    tag_name: &str,
    name: &str,
    revision: &str,
    note: Option<&str>,
) -> Result<String> {
    let mut message = format!("Checkpoint {name}");
    if let Some(note) = note {
        message.push_str("\n\n");
        message.push_str(note);
    }
    gitio::git(cwd, &["tag", "-a", "-m", &message, tag_name, revision])
        .with_context(|| format!("cannot write tag {tag_name}; nothing was recorded"))?;
    gitio::ref_value(cwd, &format!("refs/tags/{tag_name}"))?
        .with_context(|| format!("tag {tag_name} is missing after it was written"))
}

#[derive(Serialize)]
struct ChangeView {
    change_id: String,
    slug: String,
    state: String,
    patchset_id: Option<String>,
    head: Option<String>,
    /// `integrated`, `closed`, `open`, or `absent` when this ledger holds no
    /// such change.
    status: &'static str,
    /// The closure outcome of a closed change.
    outcome: Option<&'static str>,
    integrated_commit: Option<String>,
    /// Whether the integrated commit is reachable from the checkpoint
    /// revision; null when there is no integrated commit or Git could not
    /// answer.
    in_checkpoint: Option<bool>,
    /// Why Git could not answer `in_checkpoint` for an integrated commit.
    in_checkpoint_unknown: Option<String>,
    patchset_id_now: Option<String>,
    head_now: Option<String>,
}

#[derive(Serialize)]
struct LandedView {
    change_id: String,
    slug: String,
    integrated_commit: String,
    target_branch: String,
    open_at_checkpoint: bool,
}

/// An integrated change whose place relative to its target branch or the
/// checkpoint Git could not establish.
#[derive(Serialize)]
struct LandingUnknownView {
    change_id: String,
    slug: String,
    integrated_commit: String,
    target_branch: String,
    open_at_checkpoint: bool,
    reason: String,
}

#[derive(Serialize)]
struct TodoView {
    file: String,
    title: Option<String>,
    item_key: Option<String>,
    created_at: Option<DateTime<Utc>>,
    /// `consumed`, `open`, `archived` (unconsumed but out of the active
    /// journal), or `absent` when the journal no longer knows the file.
    status: &'static str,
    outcome: Option<String>,
    consumed_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct ShowView {
    schema: &'static str,
    name: String,
    event_id: String,
    recorded_at: DateTime<Utc>,
    recorded_by: String,
    revision: String,
    /// The revision recorded rewrites carry `revision` to, which ancestry is
    /// judged against.
    current_revision: String,
    tree: String,
    branch: Option<String>,
    tag: Option<String>,
    note: Option<String>,
    changes: Vec<ChangeView>,
    /// Why ancestry cannot be judged against the checkpoint at all: its
    /// revision is not in this repository. `landed_since` is then null.
    ancestry_unknown: Option<String>,
    landed_since: Option<Vec<LandedView>>,
    landed_unknown: Vec<LandingUnknownView>,
    todos: Vec<TodoView>,
    filed_since: Vec<TodoView>,
    /// Whether `filed_since` is judged against the todos the checkpoint
    /// recorded as filed. False on a checkpoint recorded without them, whose
    /// `filed_since` compares filing times and is approximate.
    filed_since_exact: bool,
}

fn todo_view(file: &str, recorded: Option<&CheckpointTodo>, now: Option<&TodoRecord>) -> TodoView {
    let status = match now {
        None => "absent",
        Some(record) if record.consumed.is_some() => "consumed",
        Some(record) if record.hot => "open",
        Some(_) => "archived",
    };
    let consumed = now.and_then(|record| record.consumed.clone());
    TodoView {
        file: file.to_string(),
        title: recorded
            .and_then(|todo| todo.title.clone())
            .or_else(|| now.and_then(|record| record.title.clone())),
        item_key: recorded
            .and_then(|todo| todo.item_key.clone())
            .or_else(|| now.and_then(|record| record.item_key.clone())),
        created_at: recorded
            .and_then(|todo| todo.created_at)
            .or_else(|| now.and_then(|record| record.created_at)),
        status,
        outcome: consumed.as_ref().map(|(outcome, _)| outcome.clone()),
        consumed_at: consumed.map(|(_, at)| at),
    }
}

fn integrated_commit(state: &ChangeState) -> Option<&str> {
    state
        .closure
        .as_ref()
        .filter(|closure| closure.outcome == Closure::Integrated)
        .and_then(|closure| closure.integrated_commit.as_deref())
}

fn show_view(ctx: &Ctx, store: &Store, checkpoint: Recorded) -> Result<ShowView> {
    let cwd = ctx.cwd.as_path();
    let current_revision = store.rewrites()?.current(&checkpoint.revision)?;
    let read = store.read()?;
    let mut states: BTreeMap<String, ChangeState> = BTreeMap::new();
    for change_id in read.change_ids()? {
        let state = read
            .state(&change_id)
            .with_context(|| format!("cannot read change {change_id}"))?;
        states.insert(change_id, state);
    }
    let ancestry_unknown = (!gitio::commit_exists(cwd, &current_revision)?)
        .then(|| format!("checkpoint revision {current_revision} is not in this repository"));
    let in_checkpoint = |commit: &str| -> Result<Ancestry> {
        match &ancestry_unknown {
            Some(reason) => Ok(Ancestry::Unanswered(reason.clone())),
            None => gitio::ancestry(cwd, commit, &current_revision),
        }
    };

    let mut changes = Vec::new();
    for recorded in &checkpoint.changes {
        let state = states.get(&recorded.change_id);
        let outcome = state
            .and_then(|state| state.closure.as_ref())
            .map(|closure| match closure.outcome {
                Closure::Integrated => "integrated",
                Closure::Abandoned => "abandoned",
                Closure::Superseded => "superseded",
            });
        let status = match (state, outcome) {
            (None, _) => "absent",
            (Some(_), Some("integrated")) => "integrated",
            (Some(_), Some(_)) => "closed",
            (Some(_), None) => "open",
        };
        let integrated = state.and_then(integrated_commit).map(str::to_string);
        let (inside, inside_unknown) = match integrated.as_deref().map(in_checkpoint).transpose()? {
            None => (None, None),
            Some(Ancestry::Ancestor) => (Some(true), None),
            Some(Ancestry::NotAncestor) => (Some(false), None),
            Some(Ancestry::Unanswered(reason)) => (None, Some(reason)),
        };
        let latest = state.and_then(ChangeState::latest_patchset);
        changes.push(ChangeView {
            change_id: recorded.change_id.clone(),
            slug: recorded.slug.clone(),
            state: recorded.state.clone(),
            patchset_id: recorded.patchset_id.clone(),
            head: recorded.head.clone(),
            status,
            outcome,
            in_checkpoint: inside,
            in_checkpoint_unknown: inside_unknown,
            integrated_commit: integrated,
            patchset_id_now: latest.map(|patchset| patchset.id.clone()),
            head_now: latest.map(|patchset| patchset.head.clone()),
        });
    }

    let open_then: BTreeSet<&str> = checkpoint
        .changes
        .iter()
        .map(|change| change.change_id.as_str())
        .collect();
    let mut landed_since = Vec::new();
    let mut landed_unknown = Vec::new();
    let mut tips: BTreeMap<String, Option<String>> = BTreeMap::new();
    let judged = if ancestry_unknown.is_some() {
        Vec::new()
    } else {
        states.values().collect()
    };
    for state in judged {
        let Some(commit) = integrated_commit(state) else {
            continue;
        };
        let target = state
            .closure
            .as_ref()
            .and_then(|closure| closure.target_branch.clone())
            .unwrap_or_else(|| state.target_branch.clone());
        let tip = match tips.get(&target) {
            Some(tip) => tip.clone(),
            None => {
                let tip = gitio::ref_value(cwd, &format!("refs/heads/{target}"))?;
                tips.insert(target.clone(), tip.clone());
                tip
            }
        };
        let Some(tip) = tip else {
            continue;
        };
        let open_at_checkpoint = open_then.contains(state.change_id.as_str());
        let unknown = |reason: String| LandingUnknownView {
            change_id: state.change_id.clone(),
            slug: state.slug.clone(),
            integrated_commit: commit.to_string(),
            target_branch: target.clone(),
            open_at_checkpoint,
            reason,
        };
        match gitio::ancestry(cwd, commit, &tip)? {
            Ancestry::NotAncestor => continue,
            Ancestry::Unanswered(reason) => {
                landed_unknown.push(unknown(reason));
                continue;
            }
            Ancestry::Ancestor => {}
        }
        match in_checkpoint(commit)? {
            Ancestry::Ancestor => {}
            Ancestry::Unanswered(reason) => landed_unknown.push(unknown(reason)),
            Ancestry::NotAncestor => landed_since.push(LandedView {
                change_id: state.change_id.clone(),
                slug: state.slug.clone(),
                integrated_commit: commit.to_string(),
                target_branch: target,
                open_at_checkpoint,
            }),
        }
    }

    let records: BTreeMap<String, TodoRecord> = journal::todo_records(cwd)?
        .into_iter()
        .map(|record| (record.file.clone(), record))
        .collect();
    let todos = checkpoint
        .todos
        .iter()
        .map(|todo| todo_view(&todo.file, Some(todo), records.get(&todo.file)))
        .collect();
    let filed_since = records
        .values()
        .filter(|record| filed_after(&checkpoint, record))
        .map(|record| todo_view(&record.file, None, Some(record)))
        .collect();

    Ok(ShowView {
        schema: "arc-checkpoint/1",
        name: checkpoint.name,
        event_id: checkpoint.event.event_id.clone(),
        recorded_at: checkpoint.event.created_at,
        recorded_by: checkpoint.event.actor.clone(),
        revision: checkpoint.revision,
        current_revision,
        tree: checkpoint.tree,
        branch: checkpoint.branch,
        tag: checkpoint.tag,
        note: checkpoint.note,
        changes,
        landed_since: ancestry_unknown.is_none().then_some(landed_since),
        ancestry_unknown,
        landed_unknown,
        todos,
        filed_since,
        filed_since_exact: checkpoint.filed_todos.is_some(),
    })
}

/// Whether a todo was filed after the checkpoint. A checkpoint that recorded
/// the todos filed by then answers exactly; an older one compares the
/// todo's filing time, in whole seconds, with its own, which can count a
/// todo filed in the checkpoint's second as filed after it.
fn filed_after(checkpoint: &Recorded, record: &TodoRecord) -> bool {
    if let Some(filed) = &checkpoint.filed_todos {
        return !filed.contains(&record.file);
    }
    let since = checkpoint.event.created_at.trunc_subsecs(0);
    !checkpoint.todos.iter().any(|todo| todo.file == record.file)
        && record.created_at.is_some_and(|created| created >= since)
}

pub fn show_checkpoint(ctx: &Ctx, name: String, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let checkpoint = find(&store, &name)?;
    let view = show_view(ctx, &store, checkpoint)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    println!("checkpoint: {}", view.name);
    let mut revision = format!("revision: {}", view.revision);
    if let Some(branch) = &view.branch {
        revision.push_str(&format!(" on {branch}"));
    }
    println!("{revision}");
    if view.current_revision != view.revision {
        println!("rewritten to: {}", view.current_revision);
    }
    println!("tree: {}", view.tree);
    if let Some(tag) = &view.tag {
        println!("tag: {tag}");
    }
    println!(
        "recorded: {} by {} (event {})",
        view.recorded_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        view.recorded_by,
        view.event_id
    );
    if let Some(note) = &view.note {
        println!("note: {note}");
    }
    println!();
    println!("changes open at the checkpoint:");
    if view.changes.is_empty() {
        println!("  (none)");
    }
    for change in &view.changes {
        let since = match change.status {
            "integrated" => format!(
                "integrated at {}{}",
                change
                    .integrated_commit
                    .as_deref()
                    .map(short_sha)
                    .unwrap_or_else(|| "?".into()),
                match (change.in_checkpoint, &change.in_checkpoint_unknown) {
                    (Some(true), _) => " (inside the checkpoint)".to_string(),
                    (_, Some(reason)) => format!(" (inside the checkpoint unknown: {reason})"),
                    _ => String::new(),
                }
            ),
            "closed" => format!("closed {}", change.outcome.unwrap_or("?")),
            "absent" => "absent from this ledger".to_string(),
            _ => {
                let then = change
                    .head
                    .as_deref()
                    .map(short_sha)
                    .unwrap_or_else(|| "none".into());
                let now = change
                    .head_now
                    .as_deref()
                    .map(short_sha)
                    .unwrap_or_else(|| "none".into());
                format!("still open, head {then} then, {now} now")
            }
        };
        println!("  {} {}: {since}", change.change_id, change.slug);
    }
    match &view.landed_since {
        None => println!(
            "landed since: unknown: {}",
            view.ancestry_unknown.as_deref().unwrap_or("?")
        ),
        Some(landed_since) => render_landed(landed_since),
    }
    if !view.landed_unknown.is_empty() {
        println!("landing unknown:");
    }
    for unknown in &view.landed_unknown {
        println!(
            "  {} {} at {} into {}: {}",
            unknown.change_id,
            unknown.slug,
            short_sha(&unknown.integrated_commit),
            unknown.target_branch,
            unknown.reason
        );
    }
    println!("todos open at the checkpoint:");
    render_todos(&view.todos);
    if view.filed_since_exact {
        println!("filed since (not part of the checkpoint):");
    } else {
        println!("filed since (not part of the checkpoint; approximate, by filing time):");
    }
    render_todos(&view.filed_since);
    Ok(())
}

fn render_landed(landed_since: &[LandedView]) {
    println!("landed since:");
    if landed_since.is_empty() {
        println!("  (none)");
    }
    for landed in landed_since {
        println!(
            "  {} {} at {} into {}{}",
            landed.change_id,
            landed.slug,
            short_sha(&landed.integrated_commit),
            landed.target_branch,
            if landed.open_at_checkpoint {
                ""
            } else {
                " (not open at the checkpoint)"
            }
        );
    }
}

fn render_todos(todos: &[TodoView]) {
    if todos.is_empty() {
        println!("  (none)");
    }
    for todo in todos {
        let standing = match (todo.status, &todo.outcome, todo.consumed_at) {
            ("consumed", Some(outcome), Some(at)) => format!(
                "consumed {outcome} at {}",
                at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            ),
            ("open", _, _) => "still open".to_string(),
            (status, _, _) => status.to_string(),
        };
        match &todo.title {
            Some(title) => println!("  {} {title}: {standing}", todo.file),
            None => println!("  {}: {standing}", todo.file),
        }
    }
}

#[derive(Serialize)]
struct ListEntry {
    name: String,
    event_id: String,
    recorded_at: DateTime<Utc>,
    revision: String,
    tag: Option<String>,
    open_changes: usize,
    open_todos: usize,
}

pub fn list_checkpoints(ctx: &Ctx, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let entries: Vec<ListEntry> = recorded(&store.load_repository_events()?)
        .into_iter()
        .map(|checkpoint| ListEntry {
            name: checkpoint.name,
            event_id: checkpoint.event.event_id.clone(),
            recorded_at: checkpoint.event.created_at,
            revision: checkpoint.revision,
            tag: checkpoint.tag,
            open_changes: checkpoint.changes.len(),
            open_todos: checkpoint.todos.len(),
        })
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": "arc-checkpoint-list/1",
                "checkpoints": entries,
            }))?
        );
        return Ok(());
    }
    if entries.is_empty() {
        println!("no checkpoints");
        return Ok(());
    }
    for entry in &entries {
        println!(
            "{} {} {}{}: {} open changes, {} open todos",
            entry.name,
            short_sha(&entry.revision),
            entry
                .recorded_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            entry
                .tag
                .as_deref()
                .map(|tag| format!(" tag {tag}"))
                .unwrap_or_default(),
            entry.open_changes,
            entry.open_todos
        );
    }
    Ok(())
}

/// The line a handoff carries for the checkpoint it names.
pub(crate) fn handoff_line(cwd: &Path, name: &str) -> Result<String> {
    let store = Store::discover_for_reading(cwd)
        .context("--checkpoint names a checkpoint in this repository's ledger")?;
    let checkpoint = find(&store, name)?;
    Ok(format!(
        "Checkpoint: {} at {}\n",
        checkpoint.name, checkpoint.revision
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{fail_directory_sync, LocalEvent};

    /// A repository holding one commit, and that commit.
    fn repository(tmp: &tempfile::TempDir) -> (std::path::PathBuf, String) {
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        for args in [
            &["init", "-q", "-b", "master"][..],
            &["config", "user.name", "Tester"],
            &["config", "user.email", "tester@example.invalid"],
            &["config", "commit.gpgsign", "false"],
            &["config", "tag.gpgsign", "false"],
            &["commit", "-q", "--allow-empty", "-m", "base"],
        ] {
            gitio::git(&root, args).unwrap();
        }
        let head = gitio::head(&root).unwrap();
        (root, head)
    }

    fn draft(name: &str, revision: &str, tag: &str) -> LocalEvent {
        LocalEvent::draft(
            serde_json::from_value(serde_json::json!({
                "schema_version": 1, "event_id": "", "repository_id": "repo",
                "change_id": Store::REPOSITORY_SCOPE, "actor": "tester",
                "created_at": chrono::Utc::now(), "event_type": "checkpoint-recorded",
                "name": name, "revision": revision, "tree": "tree", "tag": tag,
                "changes": [], "todos": [],
            }))
            .unwrap(),
        )
    }

    /// Claim: when the append publishes the checkpoint event and then fails
    /// its directory sync, the tag the event names stays, and the error says
    /// the event is published. Rejects: deleting the tag on every append
    /// error, which leaves a readable checkpoint naming a tag that is gone.
    #[test]
    fn a_published_event_whose_sync_failed_keeps_its_tag() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (root, revision) = repository(&tmp);
        let store = Store::for_tests(&tmp.path().join("store"));
        store
            .append_local_repository_event(&mut draft("warm", &revision, "checkpoint/warm"))
            .unwrap();
        let object = write_tag(&root, "checkpoint/rc", "rc", &revision, None).unwrap();
        let mut event = draft("rc", &revision, "checkpoint/rc");
        fail_directory_sync(true);
        let appended = store.append_local_repository_event(&mut event);
        fail_directory_sync(false);
        let error = appended.unwrap_err();
        assert!(store.repository_event_is_published(&event).unwrap());

        let error = failed_append(
            &store,
            &root,
            &event,
            Some(("checkpoint/rc", &object)),
            error,
        );
        assert_eq!(
            gitio::ref_value(&root, "refs/tags/checkpoint/rc").unwrap(),
            Some(object),
            "the tag of a published checkpoint was deleted: {error:#}"
        );
        let message = format!("{error:#}");
        assert!(message.contains("is published"), "{message}");
        assert!(message.contains(&event.event_id), "{message}");
    }

    /// Claim: when the append publishes nothing, the tag written for it is
    /// deleted and the error says nothing was recorded. Rejects: a tag left
    /// behind by a checkpoint that does not exist.
    #[test]
    fn an_absent_event_deletes_its_tag() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (root, revision) = repository(&tmp);
        let store = Store::for_tests(&tmp.path().join("store"));
        let object = write_tag(&root, "checkpoint/rc", "rc", &revision, None).unwrap();
        let mut event = draft("rc", &revision, "checkpoint/rc");
        event.event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV".into();
        let error = store.append_local_repository_event(&mut event).unwrap_err();
        assert!(!store.repository_event_is_published(&event).unwrap());

        let error = failed_append(
            &store,
            &root,
            &event,
            Some(("checkpoint/rc", &object)),
            error,
        );
        assert_eq!(
            gitio::ref_value(&root, "refs/tags/checkpoint/rc").unwrap(),
            None
        );
        assert!(
            format!("{error:#}").contains("nothing was recorded"),
            "{error:#}"
        );
    }

    /// Claim: a checkpoint that recorded the todos filed by then counts as
    /// filed since exactly the todos it did not record, whatever their
    /// filing times; one recorded without them judges by filing time, so a
    /// todo filed an hour before it is not filed since. Rejects: an older
    /// checkpoint read as if it had recorded no todo filed, which lists
    /// every todo the journal ever held as filed since.
    #[test]
    fn filed_since_uses_the_recorded_boundary_and_else_filing_time() {
        let event = draft("rc", "revision", "checkpoint/rc").into_event();
        let at = event.created_at;
        let record = |file: &str, created_at: DateTime<Utc>| TodoRecord {
            file: file.into(),
            title: None,
            item_key: None,
            created_at: Some(created_at),
            hot: true,
            consumed: None,
        };
        let earlier = record("earlier", at - chrono::Duration::hours(1));
        let same_second = record("same-second", at.trunc_subsecs(0));
        let later = record("later", at + chrono::Duration::hours(1));
        let mut checkpoint = Recorded {
            event,
            name: "rc".into(),
            revision: "revision".into(),
            tree: "tree".into(),
            branch: None,
            tag: None,
            changes: Vec::new(),
            todos: Vec::new(),
            filed_todos: Some(["earlier", "same-second"].map(String::from).into()),
            note: None,
        };
        assert!(!filed_after(&checkpoint, &earlier));
        assert!(!filed_after(&checkpoint, &same_second));
        assert!(filed_after(&checkpoint, &later));

        checkpoint.filed_todos = None;
        assert!(!filed_after(&checkpoint, &earlier));
        assert!(filed_after(&checkpoint, &later));
    }
}
