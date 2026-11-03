//! Checkpoints: a named revision somebody is testing, with the changes and
//! journal todos open when it was named, read back against what has happened
//! since.

use crate::gitio;
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
    let todos: Vec<CheckpointTodo> = journal::todo_records(cwd)?
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
            note: note.clone(),
        },
    )?;

    let tag_object = match &tag_name {
        Some(tag_name) => Some(write_tag(cwd, tag_name, &name, &revision, note.as_deref())?),
        None => None,
    };
    if let Err(error) = store.append_local_repository_event(&mut event) {
        if let (Some(tag_name), Some(object)) = (&tag_name, &tag_object) {
            let reference = format!("refs/tags/{tag_name}");
            return Err(
                match gitio::git(cwd, &["update-ref", "-d", &reference, object]) {
                    Ok(_) => error.context(format!(
                        "nothing was recorded; tag {tag_name} was deleted again"
                    )),
                    Err(cleanup) => error.context(format!(
                        "nothing was recorded, and tag {tag_name} could not be deleted: \
                         {cleanup:#}"
                    )),
                },
            );
        }
        return Err(error);
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
    /// revision.
    in_checkpoint: Option<bool>,
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
    landed_since: Vec<LandedView>,
    todos: Vec<TodoView>,
    filed_since: Vec<TodoView>,
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
    let in_checkpoint = |commit: &str| gitio::is_ancestor(cwd, commit, &current_revision);

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
        let latest = state.and_then(ChangeState::latest_patchset);
        changes.push(ChangeView {
            change_id: recorded.change_id.clone(),
            slug: recorded.slug.clone(),
            state: recorded.state.clone(),
            patchset_id: recorded.patchset_id.clone(),
            head: recorded.head.clone(),
            status,
            outcome,
            in_checkpoint: integrated.as_deref().map(in_checkpoint).transpose()?,
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
    let mut tips: BTreeMap<String, Option<String>> = BTreeMap::new();
    for state in states.values() {
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
        if !gitio::is_ancestor(cwd, commit, &tip)? || in_checkpoint(commit)? {
            continue;
        }
        landed_since.push(LandedView {
            change_id: state.change_id.clone(),
            slug: state.slug.clone(),
            integrated_commit: commit.to_string(),
            target_branch: target,
            open_at_checkpoint: open_then.contains(state.change_id.as_str()),
        });
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
    let part_of: BTreeSet<&str> = checkpoint
        .todos
        .iter()
        .map(|todo| todo.file.as_str())
        .collect();
    // Journal times are whole seconds; a todo filed in the checkpoint's own
    // second and absent from it was filed after it.
    let since = checkpoint.event.created_at.trunc_subsecs(0);
    let filed_since = records
        .values()
        .filter(|record| !part_of.contains(record.file.as_str()))
        .filter(|record| record.created_at.is_some_and(|created| created >= since))
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
        landed_since,
        todos,
        filed_since,
    })
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
                if change.in_checkpoint == Some(true) {
                    " (inside the checkpoint)"
                } else {
                    ""
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
    println!("landed since:");
    if view.landed_since.is_empty() {
        println!("  (none)");
    }
    for landed in &view.landed_since {
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
    println!("todos open at the checkpoint:");
    render_todos(&view.todos);
    println!("filed since (not part of the checkpoint):");
    render_todos(&view.filed_since);
    Ok(())
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
