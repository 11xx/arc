//! Workbenches: working material that outlives a session.
//!
//! A workbench is a directory under the journal's `workbench/`, holding an
//! index file arc writes and anything else a session puts there. The journal
//! records the acts on it (opening, linking, closing, archiving) and never
//! its content. Arc reads a workbench only to count its files and bytes and
//! to find when anything in it last changed, which is what its listings and
//! its staleness are made of.
//!
//! Its events carry the workbench's name and no artifact `file`, so every
//! artifact view, exchange included, passes them by.

use super::{
    append_event, archive_dir, event_identity_label, format_age, lock_journal_transition,
    parse_artifact_name, print_dir_header, read_events, relocation, resolve_dir, uniquified,
    valid_topic, DoctorFinding, JournalEvent, RecordedIdentity,
};
use crate::commands::Ctx;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The directory under the journal, and under its cold archive, that holds
/// workbenches.
const DIR: &str = "workbench";

/// The index arc writes inside every workbench.
const INDEX: &str = "WORKBENCH.md";

/// Days with no recorded act and no changed file after which an open
/// workbench is stale: the threshold `journal filed` applies to open items.
const STALE_DAYS: u64 = 14;

#[derive(clap::Args)]
pub struct WorkbenchArgs {
    #[command(subcommand)]
    command: Option<WorkbenchCmd>,
    /// Kebab-case slug; the workbench is named `<slug>-<YYYYMMDD>`
    #[arg(required = true)]
    slug: Option<String>,
    /// One line saying what the workbench is for
    #[arg(long, required = true)]
    purpose: Option<String>,
}

impl WorkbenchArgs {
    pub(super) fn writes(&self) -> bool {
        !matches!(self.command, Some(WorkbenchCmd::List { .. }))
    }
}

#[derive(Subcommand)]
pub enum WorkbenchCmd {
    /// List open workbenches with purpose, size and age (arc-journal-workbenches/1)
    ///
    /// `--all` adds closed and archived workbenches with what each produced.
    /// An open workbench is measured from its directory entries; a closed one
    /// reports the size recorded when it was closed or last moved. An open
    /// workbench is stale once 14 days pass with no recorded act and no file
    /// in it changed.
    /// Records no events; ledger reads do not require a parseable append policy.
    List {
        /// Include closed and archived workbenches
        #[arg(long)]
        all: bool,
        /// Emit structured JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Link a workbench to a change or to an artifact of this journal
    ///
    /// Records a `workbench-linked` event; a link already recorded records
    /// nothing. A change is named by its ID or a unique prefix in this
    /// project's ledger, an artifact by its filename in this journal, live or
    /// archived. Links are listed with the workbench and written to its index
    #[command(group(
        clap::ArgGroup::new("target")
            .required(true)
            .args(["change_flag", "artifact"])
    ))]
    Link {
        /// The workbench's name, or its slug while one workbench of that slug is open
        workbench: String,
        /// A change of this project's ledger
        #[arg(long, id = "change_flag")]
        change: Option<String>,
        /// An artifact filename of this journal
        #[arg(long)]
        artifact: Option<String>,
    },
    /// Close a workbench, naming what it produced
    ///
    /// Each `--produced` names a change of this project's ledger or an
    /// artifact of this journal; they are recorded on a `workbench-closed`
    /// event with the workbench's size. A workbench that produced nothing
    /// says why with `--note`; a close needs one of the two. A closed
    /// workbench leaves `catchup` and `journal open` and keeps its directory;
    /// `journal workbench list --all` lists it with what it produced, and
    /// `journal archive <name>` moves it to the cold archive. Closing twice
    /// refuses
    Close {
        /// The workbench's name, or its slug while one workbench of that slug is open
        workbench: String,
        /// A change or artifact the workbench produced; repeat for each
        #[arg(long, value_name = "CHANGE|ARTIFACT")]
        produced: Vec<String>,
        /// One line on the outcome; required when nothing was produced
        #[arg(long)]
        note: Option<String>,
    },
}

pub(super) fn run(ctx: &Ctx, args: WorkbenchArgs) -> Result<i32> {
    match args.command {
        Some(WorkbenchCmd::List { all, json }) => list(ctx, all, json),
        Some(WorkbenchCmd::Link {
            workbench,
            change,
            artifact,
        }) => link(ctx, &workbench, change.as_deref(), artifact.as_deref()),
        Some(WorkbenchCmd::Close {
            workbench,
            produced,
            note,
        }) => close(ctx, &workbench, &produced, note.as_deref()),
        None => open(
            ctx,
            args.slug.as_deref().context("a workbench needs a slug")?,
            args.purpose
                .as_deref()
                .context("a workbench needs --purpose")?,
        ),
    }
}

/// Whether an argument of `journal archive` or `unarchive` names a workbench
/// rather than an artifact: a kebab-case name, which no artifact filename is.
pub(super) fn names_workbench(argument: &str) -> bool {
    valid_topic(argument)
}

/// Whether a `workbench-*` event says what its kind needs and nothing that
/// would make an artifact view read it as its own.
pub(super) fn well_formed(event: &JournalEvent) -> bool {
    let present = |value: &Option<String>| {
        value
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty() && !value.contains(['\n', '\r']))
    };
    let unlinked = event.changes.is_empty() && event.artifacts.is_empty();
    let named = event.workbench.as_deref().is_some_and(valid_topic)
        && event.file.is_none()
        && event.changes.iter().all(|change| !change.trim().is_empty())
        && event
            .artifacts
            .iter()
            .all(|artifact| parse_artifact_name(artifact).is_some());
    named
        && match event.event.as_str() {
            "workbench-opened" => {
                present(&event.purpose) && unlinked && event.adopted_from.is_none()
            }
            "workbench-adopted" => {
                present(&event.purpose)
                    && unlinked
                    && present(&event.adopted_from)
                    && event.files.is_some()
                    && event.bytes.is_some()
            }
            "workbench-linked" => {
                event.changes.len() + event.artifacts.len() == 1 && event.purpose.is_none()
            }
            "workbench-closed" => !unlinked || present(&event.note),
            "workbench-archived" | "workbench-unarchived" => unlinked,
            _ => false,
        }
}

/// The line `journal catchup`'s tail and the event views print.
pub(super) fn event_message(event: &JournalEvent) -> String {
    let name = event.workbench.as_deref().unwrap_or_default();
    let purpose = event.purpose.as_deref().unwrap_or_default();
    match event.event.as_str() {
        "workbench-opened" => format!("opened workbench {name}: {purpose}"),
        "workbench-adopted" => format!(
            "adopted {} as workbench {name}: {purpose}",
            event.adopted_from.as_deref().unwrap_or_default()
        ),
        "workbench-linked" => format!("linked workbench {name} to {}", targets(event)),
        "workbench-closed" => {
            let produced = if event.changes.is_empty() && event.artifacts.is_empty() {
                "nothing".to_string()
            } else {
                targets(event)
            };
            format!(
                "closed workbench {name}, produced {produced}{}",
                event
                    .note
                    .as_deref()
                    .map(|note| format!(": {note}"))
                    .unwrap_or_default()
            )
        }
        "workbench-archived" => format!("archived workbench {name}"),
        "workbench-unarchived" => format!("unarchived workbench {name}"),
        _ => String::new(),
    }
}

fn targets(event: &JournalEvent) -> String {
    labelled(&event.changes, &event.artifacts)
}

fn labelled(changes: &[String], artifacts: &[String]) -> String {
    changes
        .iter()
        .map(|change| format!("change {change}"))
        .chain(
            artifacts
                .iter()
                .map(|artifact| format!("artifact {artifact}")),
        )
        .collect::<Vec<_>>()
        .join(", ")
}

/// One workbench as the journal's events record it.
struct Recorded<'a> {
    name: String,
    slug: String,
    opening: &'a JournalEvent,
    changes: Vec<String>,
    artifacts: Vec<String>,
    closing: Option<&'a JournalEvent>,
    archived: bool,
    /// The latest event recording an act on it: its size, when the act
    /// measured one, and its newest timestamp.
    sized: Option<&'a JournalEvent>,
    last_act: Option<DateTime<Utc>>,
}

impl Recorded<'_> {
    fn is_open(&self) -> bool {
        self.closing.is_none()
    }

    fn purpose(&self) -> &str {
        self.opening.purpose.as_deref().unwrap_or_default()
    }
}

/// Every recorded workbench, in the order it was opened. An act on a name no
/// opening recorded, or a second opening of one, records nothing.
fn recorded(events: &[JournalEvent]) -> Vec<Recorded<'_>> {
    let mut workbenches: Vec<Recorded<'_>> = Vec::new();
    let mut at: HashMap<&str, usize> = HashMap::new();
    for event in events {
        let Some(name) = event.workbench.as_deref() else {
            continue;
        };
        if matches!(
            event.event.as_str(),
            "workbench-opened" | "workbench-adopted"
        ) {
            if at.contains_key(name) {
                continue;
            }
            at.insert(name, workbenches.len());
            workbenches.push(Recorded {
                name: name.to_string(),
                slug: event.topic.clone(),
                opening: event,
                changes: Vec::new(),
                artifacts: Vec::new(),
                closing: None,
                archived: false,
                sized: event.files.is_some().then_some(event),
                last_act: event.timestamp(),
            });
            continue;
        }
        let Some(&index) = at.get(name) else {
            continue;
        };
        let workbench = &mut workbenches[index];
        match event.event.as_str() {
            "workbench-linked" => {
                for change in &event.changes {
                    if !workbench.changes.contains(change) {
                        workbench.changes.push(change.clone());
                    }
                }
                for artifact in &event.artifacts {
                    if !workbench.artifacts.contains(artifact) {
                        workbench.artifacts.push(artifact.clone());
                    }
                }
            }
            "workbench-closed" if workbench.closing.is_none() => workbench.closing = Some(event),
            "workbench-archived" => workbench.archived = true,
            "workbench-unarchived" => workbench.archived = false,
            _ => continue,
        }
        if event.files.is_some() {
            workbench.sized = Some(event);
        }
        workbench.last_act = workbench.last_act.max(event.timestamp());
    }
    workbenches
}

fn hot_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(DIR).join(name)
}

fn cold_path(dir: &Path, name: &str) -> PathBuf {
    archive_dir(dir).join(DIR).join(name)
}

/// Where a workbench's directory is: where its records put it, else the
/// other storage when it is found there, else `missing` at the recorded one.
fn located(dir: &Path, workbench: &Recorded<'_>) -> (PathBuf, &'static str) {
    let hot = (hot_path(dir, &workbench.name), "hot");
    let cold = (cold_path(dir, &workbench.name), "archived");
    let (expected, other) = if workbench.archived {
        (cold, hot)
    } else {
        (hot, cold)
    };
    if expected.0.is_dir() {
        expected
    } else if other.0.is_dir() {
        other
    } else {
        (expected.0, "missing")
    }
}

/// What a walk of a workbench found, from directory entries and their
/// metadata alone.
struct Measure {
    files: u64,
    bytes: u64,
    newest: Option<SystemTime>,
}

impl Measure {
    fn touch(&mut self, metadata: &std::fs::Metadata) {
        if let Ok(modified) = metadata.modified() {
            self.newest = self.newest.max(Some(modified));
        }
    }
}

/// Count the files and bytes under `root` and find the newest modification
/// among its entries, without following a symbolic link or reading content.
/// The index at the root is arc's and is not counted. `visit` sees every
/// other entry first and may refuse it.
fn walk(
    root: &Path,
    mut visit: impl FnMut(&Path, &std::fs::Metadata) -> Result<()>,
) -> Result<Measure> {
    let mut measure = Measure {
        files: 0,
        bytes: 0,
        newest: None,
    };
    let metadata = std::fs::symlink_metadata(root)
        .with_context(|| format!("cannot read {}", root.display()))?;
    measure.touch(&metadata);
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))?
        {
            let entry = entry.with_context(|| format!("cannot read {}", dir.display()))?;
            if dir == root && entry.file_name() == INDEX {
                continue;
            }
            let path = entry.path();
            let metadata = entry
                .metadata()
                .with_context(|| format!("cannot read {}", path.display()))?;
            visit(&path, &metadata)?;
            measure.touch(&metadata);
            if metadata.is_dir() {
                pending.push(path);
            } else {
                measure.files += 1;
                if metadata.is_file() {
                    measure.bytes += metadata.len();
                }
            }
        }
    }
    Ok(measure)
}

/// One workbench as the listings show it.
#[derive(Serialize)]
pub(crate) struct WorkbenchRow {
    name: String,
    slug: String,
    purpose: String,
    /// `open` or `closed`.
    state: &'static str,
    /// `hot`, `archived`, or `missing`: where the directory is now.
    storage: &'static str,
    path: String,
    opened_at: String,
    opened_by: RecordedIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    adopted_from: Option<String>,
    /// Seconds since it was opened or adopted.
    age_seconds: u64,
    /// Files and bytes: measured now for an open workbench, as last recorded
    /// for a closed one; null when neither is available.
    files: Option<u64>,
    bytes: Option<u64>,
    /// Why an open workbench could not be measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    size_error: Option<String>,
    /// The later of its last recorded act and, while it is open, the newest
    /// change to anything in it.
    last_activity: Option<String>,
    /// Open, and untouched for longer than `stale_days`.
    stale: bool,
    changes: Vec<String>,
    artifacts: Vec<String>,
    closed: Option<Closure>,
    #[serde(skip)]
    untouched_seconds: u64,
}

/// What closing a workbench recorded.
#[derive(Serialize)]
struct Closure {
    at: String,
    by: RecordedIdentity,
    changes: Vec<String>,
    artifacts: Vec<String>,
    note: Option<String>,
}

fn rows(dir: &Path, events: &[JournalEvent], all: bool, now: DateTime<Utc>) -> Vec<WorkbenchRow> {
    let mut rows: Vec<WorkbenchRow> = recorded(events)
        .into_iter()
        .filter(|workbench| all || workbench.is_open())
        .map(|workbench| row(dir, &workbench, now))
        .collect();
    rows.reverse();
    rows
}

fn row(dir: &Path, workbench: &Recorded<'_>, now: DateTime<Utc>) -> WorkbenchRow {
    let (path, storage) = located(dir, workbench);
    let seconds_since =
        |at: DateTime<Utc>| now.signed_duration_since(at).num_seconds().max(0) as u64;
    let mut last_activity = workbench.last_act;
    let (files, bytes, size_error) = if workbench.is_open() && storage == "missing" {
        (
            None,
            None,
            Some(format!("no directory at {}", path.display())),
        )
    } else if workbench.is_open() {
        match walk(&path, |_, _| Ok(())) {
            Ok(measure) => {
                last_activity = last_activity.max(measure.newest.map(DateTime::<Utc>::from));
                (Some(measure.files), Some(measure.bytes), None)
            }
            Err(error) => (None, None, Some(format!("{error:#}"))),
        }
    } else {
        let sized = workbench.sized;
        (
            sized.and_then(|event| event.files),
            sized.and_then(|event| event.bytes),
            None,
        )
    };
    let untouched_seconds = last_activity.map(seconds_since).unwrap_or_default();
    WorkbenchRow {
        name: workbench.name.clone(),
        slug: workbench.slug.clone(),
        purpose: workbench.purpose().to_string(),
        state: if workbench.is_open() {
            "open"
        } else {
            "closed"
        },
        storage,
        path: path.display().to_string(),
        opened_at: workbench.opening.ts.clone(),
        opened_by: RecordedIdentity::from(workbench.opening),
        adopted_from: workbench.opening.adopted_from.clone(),
        age_seconds: workbench
            .opening
            .timestamp()
            .map(seconds_since)
            .unwrap_or_default(),
        files,
        bytes,
        size_error,
        last_activity: last_activity
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        stale: workbench.is_open() && untouched_seconds > STALE_DAYS * 86_400,
        changes: workbench.changes.clone(),
        artifacts: workbench.artifacts.clone(),
        closed: workbench.closing.map(|event| Closure {
            at: event.ts.clone(),
            by: RecordedIdentity::from(event),
            changes: event.changes.clone(),
            artifacts: event.artifacts.clone(),
            note: event.note.clone(),
        }),
        untouched_seconds,
    }
}

/// The open workbenches of the journal at `dir`, newest first, for the views
/// that list them beside the queue.
pub(crate) fn open_rows(dir: &Path) -> Result<Vec<WorkbenchRow>> {
    Ok(rows(dir, &read_events(dir)?, false, Utc::now()))
}

fn render_row(row: &WorkbenchRow) {
    let size = match (row.files, row.bytes) {
        (Some(files), Some(bytes)) => format!(
            "{} in {files} {}",
            crate::worktree_usage::human(bytes),
            if files == 1 { "file" } else { "files" }
        ),
        _ => "size unknown".to_string(),
    };
    let mut facts = vec![size, format!("opened {} ago", format_age(row.age_seconds))];
    if row.stale {
        facts.push(format!(
            "untouched {} (stale)",
            format_age(row.untouched_seconds)
        ));
    }
    if row.storage != "hot" {
        facts.push(row.storage.to_string());
    }
    println!("  {}  {}", row.name, facts.join(", "));
    println!("    purpose: {}", row.purpose);
    if let Some(from) = &row.adopted_from {
        println!("    adopted from: {from}");
    }
    if !row.changes.is_empty() || !row.artifacts.is_empty() {
        println!("    linked: {}", labelled(&row.changes, &row.artifacts));
    }
    if let Some(closed) = &row.closed {
        let produced = if closed.changes.is_empty() && closed.artifacts.is_empty() {
            "nothing".to_string()
        } else {
            labelled(&closed.changes, &closed.artifacts)
        };
        println!("    closed {}, produced {produced}", closed.at);
        if let Some(note) = &closed.note {
            println!("    note: {note}");
        }
    }
    if let Some(error) = &row.size_error {
        println!("    size: {error}");
    }
    println!("    {}", row.path);
}

/// The open workbenches as `journal open` lists them, after the queue tiers.
pub(super) fn render_open(rows: &[WorkbenchRow]) {
    println!("workbenches (open, newest first):");
    if rows.is_empty() {
        println!("  (none)");
    }
    for row in rows {
        render_row(row);
    }
}

/// The open workbenches as `catchup` lists them beside the lanes, with the
/// count of stale ones, the way stale filed items are counted.
pub(super) fn render_catchup(rows: &[WorkbenchRow]) {
    if rows.is_empty() {
        return;
    }
    println!("workbenches ({} open):", rows.len());
    for row in rows {
        render_row(row);
    }
    let stale = rows.iter().filter(|row| row.stale).count();
    if stale > 0 {
        println!(
            "{stale} open workbenches untouched for >{STALE_DAYS} days: close each one that is done with `arc journal workbench close`"
        );
    }
}

fn list(ctx: &Ctx, all: bool, json: bool) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let rows = rows(&dir, &read_events(&dir)?, all, Utc::now());
    if json {
        #[derive(Serialize)]
        struct Listing<'a> {
            schema: &'static str,
            dir: String,
            stale_days: u64,
            workbenches: &'a [WorkbenchRow],
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Listing {
                schema: "arc-journal-workbenches/1",
                dir: dir.display().to_string(),
                stale_days: STALE_DAYS,
                workbenches: &rows,
            })?
        );
        return Ok(0);
    }
    print_dir_header(&dir);
    println!(
        "workbenches ({}{}):",
        rows.len(),
        if all { "" } else { " open" }
    );
    if rows.is_empty() {
        println!("  (none)");
    }
    for row in &rows {
        render_row(row);
    }
    Ok(0)
}

fn one_line(value: &str, flag: &str) -> Result<String> {
    if value.contains(['\n', '\r']) {
        bail!("{flag} must be a single line");
    }
    let value = value.trim();
    if value.is_empty() {
        bail!("{flag} must say something");
    }
    Ok(value.to_string())
}

fn require_slug(slug: &str) -> Result<()> {
    if !valid_topic(slug) {
        bail!(
            "workbench slug {slug:?} must be kebab-case: lowercase letters, digits and single hyphens"
        );
    }
    Ok(())
}

/// The first free name in the series `<base>`, `<base>-2`, ...: free of
/// every workbench recorded and of every directory in either storage.
fn allocate_name(dir: &Path, recorded: &[Recorded<'_>], base: &str) -> Result<String> {
    uniquified(base, |candidate| {
        recorded.iter().any(|workbench| workbench.name == candidate)
            || std::fs::symlink_metadata(hot_path(dir, candidate)).is_ok()
            || std::fs::symlink_metadata(cold_path(dir, candidate)).is_ok()
    })
}

/// The refusal for opening or adopting `slug` while a workbench of it is
/// open: two open workbenches of one slug would make the slug name neither.
fn refuse_open_slug(recorded: &[Recorded<'_>], slug: &str) -> Result<()> {
    if let Some(open) = recorded
        .iter()
        .find(|workbench| workbench.slug == slug && workbench.is_open())
    {
        bail!(
            "{slug} is already open as workbench {} for {:?}; work in it, or close it with `arc journal workbench close {}` first",
            open.name,
            open.purpose(),
            open.name
        );
    }
    Ok(())
}

/// The workbench an argument names: its name exactly, or else its slug while
/// one workbench of that slug is open, or the only one ever recorded.
fn resolve_address<'r, 'e>(
    recorded: &'r [Recorded<'e>],
    address: &str,
) -> Result<&'r Recorded<'e>> {
    if let Some(named) = recorded.iter().find(|workbench| workbench.name == address) {
        return Ok(named);
    }
    let of_slug: Vec<&Recorded<'_>> = recorded
        .iter()
        .filter(|workbench| workbench.slug == address)
        .collect();
    let open: Vec<&Recorded<'_>> = of_slug
        .iter()
        .copied()
        .filter(|workbench| workbench.is_open())
        .collect();
    match (open.as_slice(), of_slug.as_slice()) {
        ([only], _) | ([], [only]) => Ok(only),
        ([], []) => bail!(
            "no workbench named {address} in this journal; `arc journal workbench list --all` names them"
        ),
        _ => bail!(
            "{address} names several workbenches ({}); name one",
            of_slug
                .iter()
                .map(|workbench| workbench.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Refuse a name that is not an artifact this journal holds now.
fn require_artifact(dir: &Path, events: &[JournalEvent], file: &str) -> Result<()> {
    if parse_artifact_name(file).is_none() {
        bail!("{file:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    }
    if relocation(events, file).is_some() {
        bail!("{file} was relocated out of this journal");
    }
    if !dir.join(file).is_file() && !archive_dir(dir).join(file).is_file() {
        bail!(
            "no artifact {file} in {} or its cold archive",
            dir.display()
        );
    }
    Ok(())
}

/// Append one act and rewrite the workbench's index from the records that
/// now include it. The act stands once appended; an index that cannot be
/// rewritten is reported and rewritten at the next act.
fn record(
    ctx: &Ctx,
    dir: &Path,
    events: &[JournalEvent],
    event: JournalEvent,
    path: &Path,
) -> Result<()> {
    append_event(ctx, dir, &event)?;
    let mut events = events.to_vec();
    events.push(event);
    if let Err(error) = write_index(&events, path) {
        eprintln!("warning: {error:#}; the next recorded act rewrites it");
    }
    Ok(())
}

fn write_index(events: &[JournalEvent], path: &Path) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("a workbench path ends in its name")?;
    let recorded = recorded(events);
    let workbench = recorded
        .iter()
        .find(|workbench| workbench.name == name)
        .context("a recorded act names a recorded workbench")?;
    super::publish_exchange_file(&path.join(INDEX), render_index(workbench).as_bytes())
}

/// The index arc keeps in a workbench: what its records say, for a reader
/// standing in the directory.
fn render_index(workbench: &Recorded<'_>) -> String {
    let by = |event: &JournalEvent| {
        let label = event_identity_label(event);
        match event.actor.as_deref() {
            Some(actor) => format!(" by {actor}, {label}"),
            None => format!(" by {label}"),
        }
    };
    let mut text = format!(
        "# Workbench {}\n\nPurpose: {}\nOpened: {}{}\n",
        workbench.name,
        workbench.purpose(),
        workbench.opening.ts,
        by(workbench.opening)
    );
    if let Some(from) = &workbench.opening.adopted_from {
        text.push_str(&format!("Adopted from: {from}\n"));
    }
    if !workbench.changes.is_empty() {
        text.push_str(&format!(
            "Linked changes: {}\n",
            workbench.changes.join(", ")
        ));
    }
    if !workbench.artifacts.is_empty() {
        text.push_str(&format!(
            "Linked artifacts: {}\n",
            workbench.artifacts.join(", ")
        ));
    }
    match workbench.closing {
        None => text.push_str("State: open\n"),
        Some(closing) => {
            text.push_str(&format!("State: closed {}{}\n", closing.ts, by(closing)));
            if closing.changes.is_empty() && closing.artifacts.is_empty() {
                text.push_str("Produced: nothing\n");
            } else {
                text.push_str(&format!(
                    "Produced: {}\n",
                    labelled(&closing.changes, &closing.artifacts)
                ));
            }
            if let Some(note) = &closing.note {
                text.push_str(&format!("Note: {note}\n"));
            }
        }
    }
    if workbench.archived {
        text.push_str("Storage: cold archive\n");
    }
    text.push_str(
        "\nArc writes this file from the journal's records of this workbench at\n\
         every recorded act, replacing any edit to it. Everything else here is\n\
         the workbench's own.\n",
    );
    text
}

fn open(ctx: &Ctx, slug: &str, purpose: &str) -> Result<i32> {
    require_slug(slug)?;
    let purpose = one_line(purpose, "--purpose")?;
    let dir = resolve_dir(&ctx.cwd)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create journal dir {}", dir.display()))?;
    let _transition = lock_journal_transition(&dir)?;
    let events = read_events(&dir)?;
    let recorded = recorded(&events);
    if let Some(open) = recorded
        .iter()
        .find(|workbench| workbench.slug == slug && workbench.is_open())
    {
        if open.purpose() == purpose {
            println!("already open: {}", open.name);
            println!("{}", located(&dir, open).0.display());
            return Ok(0);
        }
    }
    refuse_open_slug(&recorded, slug)?;
    let now = Utc::now();
    let name = allocate_name(&dir, &recorded, &format!("{slug}-{}", now.format("%Y%m%d")))?;
    let parent = dir.join(DIR);
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;
    let path = parent.join(&name);
    std::fs::create_dir(&path).with_context(|| format!("cannot create {}", path.display()))?;
    let mut event = JournalEvent::base(ctx, now, slug, "workbench-opened");
    event.workbench = Some(name.clone());
    event.purpose = Some(purpose);
    if let Err(error) = record(ctx, &dir, &events, event, &path) {
        let _ = std::fs::remove_file(path.join(INDEX));
        let _ = std::fs::remove_dir(&path);
        return Err(error);
    }
    println!("opened: {name}");
    println!("{}", path.display());
    Ok(0)
}

fn link(ctx: &Ctx, address: &str, change: Option<&str>, artifact: Option<&str>) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let change = change
        .map(|change| ctx.store()?.resolve_change(change))
        .transpose()?;
    let _transition = lock_journal_transition(&dir)?;
    let events = read_events(&dir)?;
    let recorded = recorded(&events);
    let workbench = resolve_address(&recorded, address)?;
    let mut event = JournalEvent::base(ctx, Utc::now(), &workbench.slug, "workbench-linked");
    event.workbench = Some(workbench.name.clone());
    let target = match (change, artifact) {
        (Some(change), _) => {
            if workbench.changes.contains(&change) {
                println!("already linked: {} to change {change}", workbench.name);
                return Ok(0);
            }
            event.changes = vec![change.clone()];
            format!("change {change}")
        }
        (None, Some(artifact)) => {
            require_artifact(&dir, &events, artifact)?;
            if workbench.artifacts.iter().any(|linked| linked == artifact) {
                println!("already linked: {} to artifact {artifact}", workbench.name);
                return Ok(0);
            }
            event.artifacts = vec![artifact.to_string()];
            format!("artifact {artifact}")
        }
        (None, None) => bail!("name a --change or an --artifact to link"),
    };
    let (path, _) = located(&dir, workbench);
    record(ctx, &dir, &events, event, &path)?;
    println!("linked: {} to {target}", workbench.name);
    Ok(0)
}

fn close(ctx: &Ctx, address: &str, produced: &[String], note: Option<&str>) -> Result<i32> {
    let note = note.map(|note| one_line(note, "--note")).transpose()?;
    if produced.is_empty() && note.is_none() {
        bail!(
            "a close names what the workbench produced with --produced <change|artifact>, or says why it produced nothing with --note"
        );
    }
    let dir = resolve_dir(&ctx.cwd)?;
    let mut changes = Vec::new();
    let mut named_artifacts = Vec::new();
    let mut store = None;
    for item in produced {
        if parse_artifact_name(item).is_some() {
            named_artifacts.push(item.clone());
            continue;
        }
        let ledger = match &store {
            Some(ledger) => ledger,
            None => store.insert(ctx.store().with_context(|| {
                format!("{item} is not an artifact name, and no ledger is here to name a change")
            })?),
        };
        let change = ledger.resolve_change(item).with_context(|| {
            format!("{item} names neither an artifact of this journal nor a change of this project's ledger")
        })?;
        if !changes.contains(&change) {
            changes.push(change);
        }
    }
    let _transition = lock_journal_transition(&dir)?;
    let events = read_events(&dir)?;
    let mut artifacts = Vec::new();
    for artifact in named_artifacts {
        require_artifact(&dir, &events, &artifact)?;
        if !artifacts.contains(&artifact) {
            artifacts.push(artifact);
        }
    }
    let recorded = recorded(&events);
    let workbench = resolve_address(&recorded, address)?;
    if let Some(closing) = workbench.closing {
        bail!("{} was closed at {}", workbench.name, closing.ts);
    }
    let (path, storage) = located(&dir, workbench);
    let mut event = JournalEvent::base(ctx, Utc::now(), &workbench.slug, "workbench-closed");
    event.workbench = Some(workbench.name.clone());
    event.changes = changes;
    event.artifacts = artifacts;
    event.note = note;
    if storage != "missing" {
        let measure = walk(&path, |_, _| Ok(()))?;
        event.files = Some(measure.files);
        event.bytes = Some(measure.bytes);
    }
    let produced = if event.changes.is_empty() && event.artifacts.is_empty() {
        "nothing".to_string()
    } else {
        targets(&event)
    };
    if storage == "missing" {
        append_event(ctx, &dir, &event)?;
    } else {
        record(ctx, &dir, &events, event, &path)?;
    }
    println!("closed: {}, produced {produced}", workbench.name);
    Ok(0)
}

/// Move a closed workbench to the cold archive.
pub(super) fn archive(ctx: &Ctx, address: &str, note: Option<&str>) -> Result<i32> {
    let note = note.map(|note| one_line(note, "--note")).transpose()?;
    move_storage(ctx, address, true, note)
}

/// Bring an archived workbench back to the live journal.
pub(super) fn unarchive(ctx: &Ctx, address: &str) -> Result<i32> {
    move_storage(ctx, address, false, None)
}

fn move_storage(ctx: &Ctx, address: &str, to_cold: bool, note: Option<String>) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let events = read_events(&dir)?;
    let recorded = recorded(&events);
    let workbench = resolve_address(&recorded, address).with_context(|| {
        format!("{address} is neither a journal artifact filename nor a workbench")
    })?;
    let name = &workbench.name;
    if to_cold && workbench.is_open() {
        bail!(
            "{name} is open; close it with `arc journal workbench close {name}`, naming what it produced, before archiving it"
        );
    }
    if workbench.archived == to_cold {
        bail!(
            "{name} is already {}",
            if to_cold { "archived" } else { "live" }
        );
    }
    let (source, destination) = if to_cold {
        (hot_path(&dir, name), cold_path(&dir, name))
    } else {
        (cold_path(&dir, name), hot_path(&dir, name))
    };
    if !source.is_dir() {
        bail!("{name} has no directory at {}", source.display());
    }
    if std::fs::symlink_metadata(&destination).is_ok() {
        bail!(
            "{} already exists; move it aside first",
            destination.display()
        );
    }
    let parent = destination
        .parent()
        .context("a workbench path has a parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;
    refuse_other_filesystem(&source, parent)?;
    std::fs::rename(&source, &destination).with_context(|| {
        format!(
            "cannot move {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    let measure = walk(&destination, |_, _| Ok(()));
    let kind = if to_cold {
        "workbench-archived"
    } else {
        "workbench-unarchived"
    };
    let mut event = JournalEvent::base(ctx, Utc::now(), &workbench.slug, kind);
    event.workbench = Some(name.clone());
    event.note = note;
    if let Ok(measure) = &measure {
        event.files = Some(measure.files);
        event.bytes = Some(measure.bytes);
    }
    if let Err(error) = record(ctx, &dir, &events, event, &destination) {
        let _ = std::fs::rename(&destination, &source);
        return Err(error);
    }
    println!(
        "{}: {name}",
        if to_cold { "archived" } else { "unarchived" }
    );
    println!("{}", destination.display());
    Ok(0)
}

/// Refuse a move no single rename can make: `path` and `parent` on two
/// filesystems.
fn refuse_other_filesystem(path: &Path, parent: &Path) -> Result<()> {
    let from = std::fs::symlink_metadata(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let to =
        std::fs::metadata(parent).with_context(|| format!("cannot read {}", parent.display()))?;
    if from.dev() != to.dev() {
        bail!(
            "{} is on another filesystem than {}, so no rename can move it there atomically; move it onto that filesystem first",
            path.display(),
            parent.display()
        );
    }
    Ok(())
}

/// Report workbenches whose directories and records disagree: a recorded
/// workbench with no directory, or in the other storage, and a directory
/// under `workbench/` that no event records.
pub(super) fn inspect(
    dir: &Path,
    events: &[JournalEvent],
    problems: &mut Vec<DoctorFinding>,
) -> Result<()> {
    let recorded = recorded(events);
    let names: HashSet<&str> = recorded
        .iter()
        .map(|workbench| workbench.name.as_str())
        .collect();
    for workbench in &recorded {
        let (path, storage) = located(dir, workbench);
        if storage == "missing" {
            problems.push(DoctorFinding {
                code: "missing-workbench",
                detail: format!("{}: no directory at {}", workbench.name, path.display()),
            });
        } else if (storage == "archived") != workbench.archived {
            problems.push(DoctorFinding {
                code: "misplaced-workbench",
                detail: format!(
                    "{}: recorded {}, found at {}",
                    workbench.name,
                    if workbench.archived {
                        "archived"
                    } else {
                        "live"
                    },
                    path.display()
                ),
            });
        }
    }
    for (root, cold) in [(dir.join(DIR), false), (archive_dir(dir).join(DIR), true)] {
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", root.display()))
            }
        };
        let mut unrecorded: Vec<(PathBuf, bool)> = Vec::new();
        for entry in entries {
            let entry = entry.with_context(|| format!("cannot read {}", root.display()))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !names.contains(name.as_str()) {
                unrecorded.push((entry.path(), entry.file_type()?.is_dir()));
            }
        }
        unrecorded.sort();
        for (path, is_dir) in unrecorded {
            let remedy = if !cold && is_dir {
                "; remove it, or record it as a workbench"
            } else {
                ""
            };
            problems.push(DoctorFinding {
                code: "unrecorded-workbench",
                detail: format!("{}: no event records it{remedy}", path.display()),
            });
        }
    }
    Ok(())
}
