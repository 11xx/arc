//! Workbenches: working material that outlives a session.
//!
//! A workbench is a directory under the journal's `workbench/`, holding an
//! index file arc writes and anything else a session puts there. The journal
//! records the acts on it (opening or adopting, linking, closing, archiving)
//! and never its content. Arc reads a workbench only to count its files and
//! bytes and to find when anything in it last changed, which is what its
//! listings and its staleness are made of.
//!
//! Its events carry the workbench's name and no artifact `file`, so every
//! artifact view, exchange included, passes them by.
//!
//! Moves guard against mistakes and accidents, not against a process that
//! deliberately swaps directory entries during the call, which no rename
//! here can prevent: after every move arc confirms that what arrived is the
//! object checked and that the directory it moved into is still the
//! journal's, and otherwise stops as moved and not recorded.

use super::{
    append_event, archive_dir, event_identity_label, format_age, lock_journal_transition,
    parse_artifact_name, print_dir_header, read_events, relocation, resolve_dir, uniquified,
    valid_topic, DoctorFinding, JournalEvent, RecordedIdentity,
};
use crate::commands::Ctx;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use fsat::{Dir, Stat};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

mod adoption;
mod fsat;

/// The directory under the journal, and under its cold archive, that holds
/// workbenches.
const DIR: &str = "workbench";

/// The index arc writes inside every workbench.
const INDEX: &str = "WORKBENCH.md";

/// The first line of every index arc writes: what makes a `WORKBENCH.md`
/// arc's to rewrite.
const INDEX_MARKER: &str = "<!-- arc writes this index from the journal's records -->";

/// Where a rewrite of an index is written before it takes the index's
/// place, so no reader of `WORKBENCH.md` sees one half written.
const INDEX_DRAFT: &str = ".WORKBENCH.md.arc-draft";

/// Days with no recorded act and no changed file after which an open
/// workbench is stale: the threshold `journal filed` applies to open items.
const STALE_DAYS: u64 = 14;

/// The entries a listing walks in one workbench before it stops. Past it the
/// listing's size is a lower bound and staleness is judged on what was walked.
const WALK_LIMIT: u64 = 100_000;

/// The listing's entry bound; a debug build lets a test lower it.
fn walk_limit() -> u64 {
    #[cfg(debug_assertions)]
    if let Some(limit) = std::env::var("ARC_TEST_WORKBENCH_WALK_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        return limit;
    }
    WALK_LIMIT
}

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
    /// in it changed. The walk of an open workbench stops after 100,000
    /// entries: its size is then a lower bound, shown as `at least` and
    /// marked by `size_partial`, and its staleness is unknown, `stale` null
    /// and shown as `staleness unknown`, since what the walk did not see may
    /// have changed.
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
    /// Adopt an existing directory or file as a workbench, moving it into the journal
    ///
    /// A directory is renamed into `workbench/` whole; a file is moved into a
    /// workbench directory of its own. A rename keeps every byte, mode and
    /// time of the material, and arc adds only the index. The workbench is
    /// named `<slug>-<YYYYMMDD>`: the slug defaults to the path's name
    /// without its extension, and a name ending in `-YYYYMMDD` keeps that
    /// date. Records a `workbench-adopted` event with where the material was,
    /// its files and its bytes, and prints the new path as the last line.
    ///
    /// The material is opened, without following a link, once the journal's
    /// transition lock is held, and every check reads that opened object.
    /// Every refusal is decided before anything moves, and leaves the
    /// material as it was. By what the path names, adoption refuses a path
    /// that does not exist; a symbolic link, with or without a trailing
    /// slash; anything but a file or a directory; a filesystem root or a
    /// mount point; a file named `WORKBENCH.md`; and a path its record
    /// cannot state on one line of text.
    ///
    /// By where it lies: the current directory, the home directory, or a
    /// directory holding either; anything inside a `.git` or `.arc`
    /// directory; a path on another filesystem than the journal, which no
    /// rename can move atomically; a path a Git repository tracks, its name
    /// read literally; anything inside a Git repository's own directory or
    /// inside a journal; and a path that holds, or lies inside, storage arc
    /// keeps: this journal's own records, artifacts and archive, another
    /// journal, or arc's home.
    ///
    /// By what it holds: a Git repository, bare or not; a journal, registered
    /// or not, known by its bindings, its locks, or an event log beside
    /// artifacts; a mount point; a relative symbolic link leading out of it,
    /// which would point elsewhere once moved; and a `WORKBENCH.md` at its
    /// top that arc did not write. A slug already open refuses too.
    ///
    /// The move is one rename that replaces nothing, made only while the
    /// material's path still leads to the object checked; material changed
    /// after its checks refuses, and a kernel or filesystem with no such
    /// rename refuses the move. A system without `/dev/fd` or
    /// `/proc/self/fd`, through which arc reads and names the directories it
    /// opens, refuses before anything is read. `arc guide journal-workbenches` says what
    /// follows when the move is made and its record is not.
    ///
    /// A directory of the journal arc does not own, such as an entry of
    /// `loose/`, may be adopted. A directory under `workbench/` that no event
    /// records is adopted where it stands
    Adopt {
        /// The directory or file to move into the journal
        path: String,
        /// One line saying what the workbench is for
        #[arg(long)]
        purpose: String,
        /// Kebab-case slug, when the path's name is not one
        #[arg(long)]
        slug: Option<String>,
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
        Some(WorkbenchCmd::Adopt {
            path,
            purpose,
            slug,
        }) => adopt(ctx, &path, &purpose, slug.as_deref()),
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

/// A path that should have been a directory of the journal was a link or
/// a file.
fn not_journal_dir(path: &Path, error: std::io::Error) -> anyhow::Error {
    if fsat::not_a_real_dir(&error) {
        anyhow!(
            "{} is not a directory of the journal but a link or a file; arc moves nothing through it",
            path.display()
        )
    } else {
        anyhow::Error::new(error).context(format!("cannot open {}", path.display()))
    }
}

/// The journal at `dir`, or its cold archive, opened: the journal as arc
/// names it, the archive only when `<journal>-archive` is itself a
/// directory, never a link. `create` makes an absent archive.
fn open_root(dir: &Path, cold: bool, create: bool) -> Result<Dir> {
    if !cold {
        return Dir::open(dir).with_context(|| format!("cannot open {}", dir.display()));
    }
    let archive = archive_dir(dir);
    let (Some(parent), Some(name)) = (archive.parent(), archive.file_name()) else {
        bail!("{} has no directory above it", archive.display());
    };
    let above = Dir::open(parent).with_context(|| format!("cannot open {}", parent.display()))?;
    match above.open_dir(name) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            match above.make_dir(name) {
                Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
                    return Err(error)
                        .with_context(|| format!("cannot create {}", archive.display()))
                }
                _ => {}
            }
            above
                .open_dir(name)
                .map_err(|error| not_journal_dir(&archive, error))
        }
        opened => opened.map_err(|error| not_journal_dir(&archive, error)),
    }
}

/// The directory that holds workbenches in the journal or its archive,
/// made when absent, and refused unless it and the root above it are real
/// directories, so nothing arc moves passes through a link.
fn holding(dir: &Path, cold: bool) -> Result<Dir> {
    let root = open_root(dir, cold, true)?;
    let path = root_path(dir, cold).join(DIR);
    match root.open_dir(OsStr::new(DIR)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match root.make_dir(OsStr::new(DIR)) {
                Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
                    return Err(error).with_context(|| format!("cannot create {}", path.display()))
                }
                _ => {}
            }
            root.open_dir(OsStr::new(DIR))
                .map_err(|error| not_journal_dir(&path, error))
        }
        opened => opened.map_err(|error| not_journal_dir(&path, error)),
    }
}

fn root_path(dir: &Path, cold: bool) -> PathBuf {
    if cold {
        archive_dir(dir)
    } else {
        dir.to_path_buf()
    }
}

/// The workbench `name` in the journal or its archive, opened, when the
/// root, `workbench/` and the workbench are all real directories there.
fn contained(dir: &Path, cold: bool, name: &str) -> Option<Dir> {
    open_root(dir, cold, false)
        .ok()?
        .open_dir(OsStr::new(DIR))
        .ok()?
        .open_dir(OsStr::new(name))
        .ok()
}

/// Where a workbench's directory is: where its records put it, else the
/// other storage when it is found there, else `missing` at the recorded
/// one, with the directory opened when it is found. A directory reached
/// through a link is not the journal's and is missing here; `journal
/// doctor` names where it leads.
fn locate(dir: &Path, workbench: &Recorded<'_>) -> (PathBuf, &'static str, Option<Dir>) {
    let name = &workbench.name;
    let (expected, other) = if workbench.archived {
        ((true, "archived"), (false, "hot"))
    } else {
        ((false, "hot"), (true, "archived"))
    };
    for (cold, storage) in [expected, other] {
        if let Some(opened) = contained(dir, cold, name) {
            return (
                root_path(dir, cold).join(DIR).join(name),
                storage,
                Some(opened),
            );
        }
    }
    (
        root_path(dir, expected.0).join(DIR).join(name),
        "missing",
        None,
    )
}

fn located(dir: &Path, workbench: &Recorded<'_>) -> (PathBuf, &'static str) {
    let (path, storage, _) = locate(dir, workbench);
    (path, storage)
}

/// What a walk of a workbench found, from directory entries alone.
struct Measure {
    files: u64,
    bytes: u64,
    newest: Option<SystemTime>,
    /// The walk stopped at its entry limit: the counts are lower bounds.
    partial: bool,
}

impl Measure {
    fn touch(&mut self, stat: &Stat) {
        self.newest = self.newest.max(Some(stat.modified));
    }
}

/// Count the files and bytes under the opened directory `root`, which lies
/// at `root_path`, and find the newest modification among its entries,
/// without following a link or reading content, stopping after `limit`
/// entries when one is given. Each directory is opened from the one above
/// it. The index at the root is arc's and is not counted. `visit` sees
/// every other entry, with the directory holding it and its name, first,
/// and may refuse it.
fn walk(
    root: &Dir,
    root_path: &Path,
    limit: Option<u64>,
    mut visit: impl FnMut(&Path, &Dir, &OsStr, &Stat) -> Result<()>,
) -> Result<Measure> {
    let cannot = |path: &Path| format!("cannot read {}", path.display());
    let mut measure = Measure {
        files: 0,
        bytes: 0,
        newest: None,
        partial: false,
    };
    measure.touch(&root.stat().with_context(|| cannot(root_path))?);
    let start = root
        .open_dir(OsStr::new("."))
        .with_context(|| cannot(root_path))?;
    let mut seen = 0u64;
    let mut queue: Vec<(Rc<Dir>, std::ffi::OsString, PathBuf)> = Vec::new();
    let mut current = Some((Rc::new(start), root_path.to_path_buf(), true));
    while let Some((dir, path, at_root)) = current.take() {
        for name in dir.names().with_context(|| cannot(&path))? {
            let name = name.with_context(|| cannot(&path))?;
            if at_root && name == INDEX {
                continue;
            }
            if limit.is_some_and(|limit| seen >= limit) {
                measure.partial = true;
                return Ok(measure);
            }
            seen += 1;
            let entry_path = path.join(&name);
            let Some(stat) = dir.entry(&name).with_context(|| cannot(&entry_path))? else {
                continue;
            };
            visit(&entry_path, &dir, &name, &stat)?;
            measure.touch(&stat);
            if stat.is_dir() {
                queue.push((Rc::clone(&dir), name, entry_path));
            } else {
                measure.files += 1;
                if stat.is_file() {
                    measure.bytes += stat.size;
                }
            }
        }
        drop(dir);
        if let Some((parent, name, path)) = queue.pop() {
            let child = parent
                .open_dir(&name)
                .with_context(|| format!("cannot open {}", path.display()))?;
            current = Some((Rc::new(child), path, false));
        }
    }
    Ok(measure)
}

/// One workbench as the listings show it.
#[derive(Clone, Debug, Serialize)]
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
    /// The walk of an open workbench stopped at its entry limit: `files` and
    /// `bytes` are lower bounds, and staleness is judged on what was walked.
    size_partial: bool,
    /// Why an open workbench could not be measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    size_error: Option<String>,
    /// The later of its last recorded act and, while it is open, the newest
    /// change to anything in it.
    last_activity: Option<String>,
    /// Open, and untouched for longer than `stale_days`; null while the
    /// walk of an open workbench did not see all of it, since what it did
    /// not see may have changed.
    stale: Option<bool>,
    changes: Vec<String>,
    artifacts: Vec<String>,
    closed: Option<Closure>,
    #[serde(skip)]
    untouched_seconds: u64,
}

/// What closing a workbench recorded.
#[derive(Clone, Debug, Serialize)]
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
    let (path, storage, opened) = locate(dir, workbench);
    let seconds_since =
        |at: DateTime<Utc>| now.signed_duration_since(at).num_seconds().max(0) as u64;
    let mut last_activity = workbench.last_act;
    let mut size_partial = false;
    let mut walked_whole = false;
    let (files, bytes, size_error) = match (workbench.is_open(), opened) {
        (true, None) => (
            None,
            None,
            Some(format!("no directory at {}", path.display())),
        ),
        (true, Some(opened)) => {
            match walk(&opened, &path, Some(walk_limit()), |_, _, _, _| Ok(())) {
                Ok(measure) => {
                    last_activity = last_activity.max(measure.newest.map(DateTime::<Utc>::from));
                    size_partial = measure.partial;
                    walked_whole = !measure.partial;
                    (Some(measure.files), Some(measure.bytes), None)
                }
                Err(error) => (None, None, Some(format!("{error:#}"))),
            }
        }
        (false, _) => {
            let sized = workbench.sized;
            (
                sized.and_then(|event| event.files),
                sized.and_then(|event| event.bytes),
                None,
            )
        }
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
        size_partial,
        size_error,
        last_activity: last_activity
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        stale: match (workbench.is_open(), walked_whole) {
            (false, _) => Some(false),
            (true, true) => Some(untouched_seconds > STALE_DAYS * 86_400),
            (true, false) => None,
        },
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

/// The open workbenches of the journal at `dir`, newest first, from the
/// events a view has already read, for the views that list them beside the
/// queue.
pub(super) fn open_rows_in(dir: &Path, events: &[JournalEvent]) -> Vec<WorkbenchRow> {
    rows(dir, events, false, Utc::now())
}

impl WorkbenchRow {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn purpose(&self) -> &str {
        &self.purpose
    }

    pub(crate) fn is_stale(&self) -> bool {
        self.stale == Some(true)
    }

    /// Its size, age and staleness, as every listing states them.
    pub(crate) fn facts(&self) -> String {
        let mut facts = vec![
            self.size(),
            format!("opened {} ago", format_age(self.age_seconds)),
        ];
        match self.stale {
            Some(true) => facts.push(format!(
                "untouched {} (stale)",
                format_age(self.untouched_seconds)
            )),
            None if self.size_partial => facts.push(format!(
                "staleness unknown: the walk stopped at {} entries",
                walk_limit()
            )),
            None => facts.push("staleness unknown".to_string()),
            Some(false) => {}
        }
        if self.storage != "hot" {
            facts.push(self.storage.to_string());
        }
        facts.join(", ")
    }

    fn size(&self) -> String {
        match (self.files, self.bytes) {
            (Some(files), Some(bytes)) if self.size_partial => format!(
                "at least {} in {files}+ files",
                crate::worktree_usage::human(bytes)
            ),
            (Some(files), Some(bytes)) => format!(
                "{} in {files} {}",
                crate::worktree_usage::human(bytes),
                if files == 1 { "file" } else { "files" }
            ),
            _ => "size unknown".to_string(),
        }
    }
}

fn render_row(row: &WorkbenchRow) {
    println!("  {}  {}", row.name, row.facts());
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
    let stale = rows.iter().filter(|row| row.stale == Some(true)).count();
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

/// Rewrite a workbench's index from the records that now include `event`.
/// The act stands once appended; an index that cannot be rewritten is
/// reported and rewritten at the next act.
fn reindex(events: &[JournalEvent], event: JournalEvent, opened: Option<&Dir>, at: &Path) {
    let Some(opened) = opened else {
        return;
    };
    let mut events = events.to_vec();
    events.push(event);
    if let Err(error) = write_index(&events, opened, at, false) {
        eprintln!("warning: {error:#}; the next recorded act rewrites it");
    }
}

/// Write the index of the workbench opened as `opened`, which lies at
/// `at`, from `events`. A fresh index is created and never replaces
/// anything. A rewrite replaces only an index carrying arc's marker: the
/// new text is written beside it, then renamed over it while it is still
/// the file that was checked, so a reader finds the old index or the new
/// one whole, and an interrupted rewrite leaves the old one.
fn write_index(events: &[JournalEvent], opened: &Dir, at: &Path, fresh: bool) -> Result<()> {
    let name = at
        .file_name()
        .and_then(|name| name.to_str())
        .context("a workbench path ends in its name")?;
    let recorded = recorded(events);
    let workbench = recorded
        .iter()
        .find(|workbench| workbench.name == name)
        .context("a recorded act names a recorded workbench")?;
    let text = render_index(workbench);
    let index = at.join(INDEX);
    let create = || -> Result<()> {
        let mut file = opened.create_new(OsStr::new(INDEX)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                anyhow!(
                    "{} is there already, and arc writes no index over a file it did not write",
                    index.display()
                )
            } else {
                anyhow::Error::new(error).context(format!("cannot create {}", index.display()))
            }
        })?;
        file.write_all(text.as_bytes())
            .with_context(|| format!("cannot write {}", index.display()))
    };
    if fresh {
        return create();
    }
    let checked = match opened.open_file(OsStr::new(INDEX), false) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return create(),
        Err(_) => bail!(
            "{} is not a file arc wrote; arc leaves it as it is",
            index.display()
        ),
    };
    if !carries_marker(&checked) {
        bail!(
            "{} is not the index arc writes; arc leaves it as it is",
            index.display()
        );
    }
    let checked = checked
        .metadata()
        .with_context(|| format!("cannot read {}", index.display()))?;
    let draft = at.join(INDEX_DRAFT);
    write_draft(opened, &draft, &text)?;
    let published = (|| -> Result<()> {
        #[cfg(debug_assertions)]
        if std::env::var_os("ARC_TEST_WORKBENCH_INDEX_INTERRUPT").is_some() {
            std::process::exit(9);
        }
        use std::os::unix::fs::MetadataExt;
        let current = opened.entry(OsStr::new(INDEX))?;
        if !current
            .is_some_and(|current| current.dev == checked.dev() && current.ino == checked.ino())
        {
            bail!(
                "{} changed while arc rewrote it; arc leaves it as it is",
                index.display()
            );
        }
        opened
            .rename_over(OsStr::new(INDEX_DRAFT), OsStr::new(INDEX))
            .with_context(|| format!("cannot replace {}", index.display()))
    })();
    if published.is_err() {
        let _ = opened.remove_file(OsStr::new(INDEX_DRAFT));
    }
    published
}

/// Write `text` to the draft beside an index, durably, so the rename that
/// publishes it never publishes an empty file. A draft an interrupted
/// rewrite left, which carries arc's marker, is replaced; any other file
/// of that name is left as it is and stops the rewrite.
fn write_draft(opened: &Dir, draft: &Path, text: &str) -> Result<()> {
    let name = OsStr::new(INDEX_DRAFT);
    let mut file = match opened.create_new(name) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let left = opened
                .open_file(name, false)
                .is_ok_and(|file| carries_marker(&file));
            if !left {
                bail!(
                    "{} is not a draft arc wrote, and arc writes its index there first; arc leaves it as it is",
                    draft.display()
                );
            }
            opened
                .remove_file(name)
                .and_then(|()| opened.create_new(name))
        }
        created => created,
    }
    .with_context(|| format!("cannot create {}", draft.display()))?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    if let Err(error) = written {
        let _ = opened.remove_file(name);
        return Err(error).with_context(|| format!("cannot write {}", draft.display()));
    }
    Ok(())
}

/// Whether `file` begins with arc's marker line.
fn carries_marker(file: &std::fs::File) -> bool {
    let mut first = String::new();
    std::io::BufReader::new(file.take(INDEX_MARKER.len() as u64 + 1))
        .read_line(&mut first)
        .is_ok()
        && first.trim_end_matches('\n') == INDEX_MARKER
}

/// Whether the index at the top of `opened` is one arc wrote.
fn is_arc_index(opened: &Dir) -> bool {
    opened
        .open_file(OsStr::new(INDEX), false)
        .is_ok_and(|file| carries_marker(&file))
}

/// What frees a workbench at `at` whose index name holds a file arc did
/// not write, such as one made while arc worked or an index arc began and
/// never finished, which carries no marker.
fn foreign_index_step(at: &Path) -> String {
    format!(
        "{} is not an index arc wrote, and arc never overwrites, moves or renames it: move or rename it, then `arc journal workbench adopt {} --purpose TEXT` records the workbench where it stands",
        at.join(INDEX).display(),
        at.display()
    )
}

/// Whether `holding`, opened earlier, is still the directory at `path` in
/// the journal or its archive: the same device and inode there, reached
/// without a link at the archive root.
fn still_holds(dir: &Path, cold: bool, holding: &Dir) -> bool {
    if cold && !std::fs::symlink_metadata(archive_dir(dir)).is_ok_and(|metadata| metadata.is_dir())
    {
        return false;
    }
    let path = root_path(dir, cold).join(DIR);
    let (Ok(now), Ok(opened)) = (std::fs::symlink_metadata(&path), holding.stat()) else {
        return false;
    };
    use std::os::unix::fs::MetadataExt;
    now.is_dir() && now.dev() == opened.dev && now.ino() == opened.ino
}

/// What the journal holds of one act after its append reported an error.
enum Outcome {
    Recorded,
    /// Prepared and unpublished, with what publishes it.
    Prepared(String),
    Absent,
}

/// The last record naming `name` in an event log.
fn last_record<'a>(log: &'a str, name: &str) -> Option<&'a str> {
    log.lines().rev().find(|line| {
        line.contains("\"workbench\"")
            && serde_json::from_str::<serde_json::Value>(line)
                .is_ok_and(|record| record["workbench"] == name)
    })
}

/// Read what the journal at `dir` holds of `event`, to say what happened
/// and nothing more. The prepared write is read before the event log, so a
/// record published between the two reads is still found published.
fn outcome(dir: &Path, event: &JournalEvent) -> Outcome {
    let line = serde_json::to_string(event).unwrap_or_default();
    let name = event.workbench.clone().unwrap_or_default();
    let prepared = std::fs::read(super::exchange_pending_path(dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<super::ExchangeTransaction>(&bytes).ok());
    let log = std::fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
    if last_record(&log, &name) == Some(line.as_str()) {
        return Outcome::Recorded;
    }
    match prepared {
        Some(prepared) if last_record(&prepared.events, &name) == Some(line.as_str()) => {
            Outcome::Prepared(super::exchange_refusal(
                dir,
                &prepared,
                super::is_global_dir(dir).unwrap_or(false),
            ))
        }
        _ => Outcome::Absent,
    }
}

/// An act whose change on disk was made and whose record was not: the
/// state the disk is left in, which nothing undoes.
struct Unrecorded<'a> {
    /// What happened on disk: `moved`, `created`, or `nothing moved`.
    done: &'a str,
    /// Where the material is now.
    at: &'a Path,
    /// What `journal doctor` reports meanwhile.
    doctor: String,
    /// The command that records the act where the material stands.
    repair: String,
}

impl Unrecorded<'_> {
    /// The index of the workbench opened as `opened` could not be written.
    /// A file arc did not write in the index's place is what stops it, and
    /// only its owner frees the name.
    fn index_error(&self, opened: &Dir, error: anyhow::Error) -> anyhow::Error {
        if opened
            .entry(OsStr::new(INDEX))
            .is_ok_and(|entry| entry.is_some())
            && !is_arc_index(opened)
        {
            return anyhow!(
                "{}, not recorded: the material is at {}. {}",
                self.done,
                self.at.display(),
                foreign_index_step(self.at)
            );
        }
        self.error(format_args!("{error:#}"))
    }

    /// The directory the material moved into is no longer the journal's:
    /// say where the move put it, as far as it can be named, and record
    /// nothing for material outside the journal.
    fn escaped(&self, what: &Path, holding: &Dir, name: &OsStr) -> anyhow::Error {
        let place = holding.current_path().map_or_else(
            || "a directory arc can no longer name".to_string(),
            |path| path.join(name).display().to_string(),
        );
        anyhow!(
            "{}, not recorded: {} moved to {place}, because {} left the journal while arc moved into it. Nothing is recorded for material outside the journal; adopt it from where it is with `arc journal workbench adopt PATH --purpose TEXT`",
            self.done,
            what.display(),
            self.at.parent().unwrap_or(self.at).display()
        )
    }

    fn error(&self, detail: impl std::fmt::Display) -> anyhow::Error {
        anyhow!(
            "{}, not recorded: the material is at {}. {detail}. `arc journal doctor` reports {}; {}",
            self.done,
            self.at.display(),
            self.doctor,
            self.repair
        )
    }

    /// Append `event`. Recorded, or this state: an append error does not
    /// prove nothing was written, so the journal says which.
    fn record(&self, ctx: &Ctx, dir: &Path, event: &JournalEvent) -> Result<()> {
        let error = match append_event(ctx, dir, event) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        match outcome(dir, event) {
            Outcome::Recorded => {
                eprintln!(
                    "warning: the journal reported an error after recording this act: {error:#}"
                );
                Ok(())
            }
            Outcome::Prepared(remedy) => Err(self.error(format_args!(
                "Its record is prepared and not yet published ({remedy}); once published it records the material where it is: {error:#}"
            ))),
            Outcome::Absent => Err(self.error(format_args!("{error:#}"))),
        }
    }
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
        "{INDEX_MARKER}\n# Workbench {}\n\nPurpose: {}\nOpened: {}{}\n",
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
    let mut event = JournalEvent::base(ctx, now, slug, "workbench-opened");
    event.workbench = Some(name.clone());
    event.purpose = Some(purpose);
    let holding = holding(&dir, false)?;
    let path = dir.join(DIR).join(&name);
    holding
        .make_dir(OsStr::new(&name))
        .with_context(|| format!("cannot create {}", path.display()))?;
    let state = Unrecorded {
        done: "created",
        at: &path,
        doctor: format!("{} as unrecorded-workbench", path.display()),
        repair: format!(
            "`arc journal workbench adopt {} --purpose TEXT` records it where it stands",
            path.display()
        ),
    };
    let opened = holding
        .open_dir(OsStr::new(&name))
        .map_err(|error| state.error(error))?;
    if !still_holds(&dir, false, &holding) {
        return Err(state.escaped(&path, &holding, OsStr::new(&name)));
    }
    let mut with = events.clone();
    with.push(event.clone());
    write_index(&with, &opened, &path, true).map_err(|error| state.index_error(&opened, error))?;
    state.record(ctx, &dir, &event)?;
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
    append_event(ctx, &dir, &event)?;
    let (path, _, opened) = locate(&dir, workbench);
    reindex(&events, event, opened.as_ref(), &path);
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
    fsat::require_descriptor_dirs()?;
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
    let (path, _, opened) = locate(&dir, workbench);
    let mut event = JournalEvent::base(ctx, Utc::now(), &workbench.slug, "workbench-closed");
    event.workbench = Some(workbench.name.clone());
    event.changes = changes;
    event.artifacts = artifacts;
    event.note = note;
    if let Some(opened) = &opened {
        let measure = walk(opened, &path, None, |_, _, _, _| Ok(()))?;
        event.files = Some(measure.files);
        event.bytes = Some(measure.bytes);
    }
    let produced = if event.changes.is_empty() && event.artifacts.is_empty() {
        "nothing".to_string()
    } else {
        targets(&event)
    };
    append_event(ctx, &dir, &event)?;
    reindex(&events, event, opened.as_ref(), &path);
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
    fsat::require_descriptor_dirs()?;
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
    let kind = if to_cold {
        "workbench-archived"
    } else {
        "workbench-unarchived"
    };
    let command = if to_cold { "archive" } else { "unarchive" };
    let from = root_path(&dir, !to_cold).join(DIR).join(name);
    let destination = root_path(&dir, to_cold).join(DIR).join(name);
    let state = Unrecorded {
        done: "moved",
        at: &destination,
        doctor: format!("{name} as misplaced-workbench"),
        repair: format!("`arc journal {command} {name}` records it where it is"),
    };
    let (opened, moving) = match contained(&dir, !to_cold, name) {
        Some(opened) => (opened, true),
        // A move whose record was not written: the directory is already
        // where the act puts it, and only the record is missing.
        None => match contained(&dir, to_cold, name) {
            Some(opened) => (opened, false),
            None => bail!(
                "{name} has no directory of the journal at {}; `arc journal doctor` says where it is",
                from.display()
            ),
        },
    };
    let at = if moving { &from } else { &destination };
    let identity = opened.stat()?;
    let measure = walk(&opened, at, None, |_, _, _, _| Ok(()))?;
    let mut event = JournalEvent::base(ctx, Utc::now(), &workbench.slug, kind);
    event.workbench = Some(name.clone());
    event.note = note;
    event.files = Some(measure.files);
    event.bytes = Some(measure.bytes);
    if !event.known() {
        bail!("the record of this move would not be read back; nothing moved");
    }
    if moving {
        let target = holding(&dir, to_cold)?;
        let source = open_root(&dir, !to_cold, false)?
            .open_dir(OsStr::new(DIR))
            .map_err(|error| not_journal_dir(&from, error))?;
        refuse_other_filesystem(&identity, &from, &target.stat()?, &destination)?;
        if !source
            .entry(OsStr::new(name))?
            .is_some_and(|entry| entry.same(&identity))
        {
            bail!(
                "{} changed while it was measured; nothing moved",
                from.display()
            );
        }
        #[cfg(debug_assertions)]
        crate::import_authorization::test_rendezvous("workbench-archive-moving")?;
        fsat::rename_new(&source, OsStr::new(name), &target, OsStr::new(name)).with_context(
            || {
                format!(
                    "cannot move {} to {}; nothing moved",
                    from.display(),
                    destination.display()
                )
            },
        )?;
        if !still_holds(&dir, to_cold, &target) {
            return Err(state.escaped(&from, &target, OsStr::new(name)));
        }
        if !target
            .entry(OsStr::new(name))?
            .is_some_and(|entry| entry.same(&identity))
        {
            return Err(state.error(not_checked(&from)));
        }
        state.record(ctx, &dir, &event)?;
    } else {
        Unrecorded {
            done: "nothing moved",
            ..state
        }
        .record(ctx, &dir, &event)?;
    }
    let reopened = contained(&dir, to_cold, name);
    reindex(&events, event, reopened.as_ref(), &destination);
    println!("{command}d: {name}");
    println!("{}", destination.display());
    Ok(0)
}

/// Refuse a move no single rename can make: the material at `path` and the
/// directory it would move into, at `to`, on two filesystems.
fn refuse_other_filesystem(material: &Stat, path: &Path, into: &Stat, to: &Path) -> Result<()> {
    if material.dev != into.dev {
        bail!(
            "{} is on another filesystem than {}, so no rename can move it there atomically; move it onto that filesystem first",
            path.display(),
            to.parent().unwrap_or(to).display()
        );
    }
    Ok(())
}

/// A material name's slug and the date it carries, when it ends in a valid
/// `-YYYYMMDD`.
fn split_date(stem: &str) -> (&str, Option<&str>) {
    if let Some((slug, date)) = stem.rsplit_once('-') {
        if date.len() == 8
            && date.bytes().all(|byte| byte.is_ascii_digit())
            && chrono::NaiveDate::parse_from_str(date, "%Y%m%d").is_ok()
        {
            return (slug, Some(date));
        }
    }
    (stem, None)
}

fn adopt(ctx: &Ctx, raw: &str, purpose: &str, slug: Option<&str>) -> Result<i32> {
    let purpose = one_line(purpose, "--purpose")?;
    if let Some(slug) = slug {
        require_slug(slug)?;
    }
    fsat::require_descriptor_dirs()?;
    let dir = resolve_dir(&ctx.cwd)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create journal dir {}", dir.display()))?;
    // The material is opened and checked under the lock, so nothing another
    // adoption or archive does can change it between the checks.
    let _transition = lock_journal_transition(&dir)?;
    let source = adoption::resolve(ctx, raw)?;
    let events = read_events(&dir)?;
    let recorded = recorded(&events);
    let hot = std::fs::canonicalize(&dir)?;
    let journal = Dir::open(&dir).with_context(|| format!("cannot open {}", dir.display()))?;
    let in_place = source.is_dir()
        && source.path.parent() == Some(hot.join(DIR).as_path())
        && journal
            .open_dir(OsStr::new(DIR))
            .and_then(|holding| holding.stat())
            .is_ok_and(|holding| holding.same(&source.parent_stat));
    let material = source.name.to_string_lossy().to_string();
    if in_place && recorded.iter().any(|workbench| workbench.name == material) {
        bail!(
            "{} is the workbench {material} already",
            source.path.display()
        );
    }
    // Everything that can refuse is settled before anything moves.
    adoption::refuse_location(ctx, &dir, &hot, &source, in_place)?;
    if !in_place {
        refuse_other_filesystem(&source.stat, &source.path, &journal.stat()?, &hot.join(DIR))?;
    }
    let measure = adoption::survey(&source, in_place)?;
    adoption::refuse_tracked(&source)?;
    let stem = if source.is_dir() {
        material.as_str()
    } else {
        Path::new(&material)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(&material)
    };
    let (default_slug, date) = split_date(stem);
    let slug = slug.unwrap_or(default_slug);
    if !valid_topic(slug) {
        bail!("{material:?} is not a kebab-case name; name a slug with --slug");
    }
    refuse_open_slug(&recorded, slug)?;
    let name = if in_place {
        if !valid_topic(&material) {
            bail!("{material:?} is not a kebab-case name; rename the directory before adopting it");
        }
        material.clone()
    } else {
        let date = date
            .map(str::to_string)
            .unwrap_or_else(|| Utc::now().format("%Y%m%d").to_string());
        allocate_name(&dir, &recorded, &format!("{slug}-{date}"))?
    };
    let mut event = JournalEvent::base(ctx, Utc::now(), slug, "workbench-adopted");
    event.workbench = Some(name.clone());
    event.purpose = Some(purpose);
    event.adopted_from = Some(source.recorded.clone());
    event.files = Some(measure.files);
    event.bytes = Some(measure.bytes);
    if !event.known() {
        bail!(
            "the record of this adoption would not be read back; nothing moved from {}",
            source.path.display()
        );
    }
    let mut with = events.clone();
    with.push(event.clone());
    #[cfg(debug_assertions)]
    crate::import_authorization::test_rendezvous("workbench-adopt-checked")?;
    let target = if in_place {
        source.path.clone()
    } else {
        dir.join(DIR).join(&name)
    };
    let state = Unrecorded {
        done: if in_place { "nothing moved" } else { "moved" },
        at: &target,
        doctor: format!("{} as unrecorded-workbench", target.display()),
        repair: format!(
            "`arc journal workbench adopt {} --purpose TEXT` records it where it stands",
            target.display()
        ),
    };
    if in_place {
        adoption::confirm(&source)?;
        let opened = source
            .dir
            .as_ref()
            .context("an in-place adoption is a directory")?;
        let fresh = !is_arc_index(opened);
        write_index(&with, opened, &target, fresh)
            .map_err(|error| state.index_error(opened, error))?;
        state.record(ctx, &dir, &event)?;
    } else {
        let holding = holding(&dir, false)?;
        let opened = move_material(&dir, &source, &holding, &name, &target, &state)?;
        write_index(&with, &opened, &target, true)
            .map_err(|error| state.index_error(&opened, error))?;
        state.record(ctx, &dir, &event)?;
    }
    println!(
        "adopted: {} as {name}, {} in {} {}",
        source.path.display(),
        crate::worktree_usage::human(measure.bytes),
        measure.files,
        if measure.files == 1 { "file" } else { "files" }
    );
    println!("{}", target.display());
    Ok(0)
}

/// Move the checked material into the workbench `name` under `holding` by
/// one rename that replaces nothing, confirming first that the material's
/// path still leads to the object checked, and return the workbench
/// opened. A file moves into a directory made for it. Once the rename is
/// done nothing is taken back: a failure after it is `state`.
fn move_material(
    dir: &Path,
    source: &adoption::Source,
    holding: &Dir,
    name: &str,
    target: &Path,
    state: &Unrecorded<'_>,
) -> Result<Dir> {
    let cannot = |error: std::io::Error| {
        anyhow::Error::new(error).context(format!(
            "cannot move {} to {}; nothing moved",
            source.path.display(),
            target.display()
        ))
    };
    let name = OsStr::new(name);
    if source.is_dir() {
        adoption::confirm(source)?;
        #[cfg(debug_assertions)]
        crate::import_authorization::test_rendezvous("workbench-adopt-moving")?;
        fsat::rename_new(&source.parent, &source.name, holding, name).map_err(cannot)?;
        if !still_holds(dir, false, holding) {
            return Err(state.escaped(&source.path, holding, name));
        }
        let opened = holding.open_dir(name).map_err(|error| state.error(error))?;
        if !opened.stat()?.same(&source.stat) {
            return Err(state.error(not_checked(&source.path)));
        }
        return Ok(opened);
    }
    holding
        .make_dir(name)
        .with_context(|| format!("cannot create {}", target.display()))?;
    let made = holding.open_dir(name);
    let moved = made
        .as_ref()
        .map_err(|error| std::io::Error::from(error.kind()))
        .and_then(|holder| {
            adoption::confirm(source)
                .map_err(|error| std::io::Error::other(format!("{error:#}")))?;
            fsat::rename_new(&source.parent, &source.name, holder, &source.name)
        });
    if let Err(error) = moved {
        // Nothing moved: the directory made for the file is still empty.
        let _ = holding.remove_empty_dir(name);
        return Err(cannot(error));
    }
    if !still_holds(dir, false, holding) {
        return Err(state.escaped(&source.path, holding, name));
    }
    let opened = made.map_err(|error| state.error(error))?;
    if !opened
        .entry(&source.name)?
        .is_some_and(|entry| entry.same(&source.stat))
    {
        return Err(state.error(not_checked(&source.path)));
    }
    Ok(opened)
}

/// What moved from `from` once a move found it was not the object checked.
fn not_checked(from: &Path) -> String {
    format!(
        "What moved from {} is not the material that was checked, so nothing is recorded for it",
        from.display()
    )
}

/// What a sandbox clone took of a journal's workbenches.
#[derive(Debug, Default, Serialize)]
pub(crate) struct SandboxCopy {
    /// Workbench directories made in the copy, each with arc's index.
    pub(crate) directories: u64,
    /// Whether the content was copied, or left in the source.
    pub(crate) content: bool,
    /// The content, every index excluded: what was copied, or what was left.
    pub(crate) files: u64,
    pub(crate) bytes: u64,
}

impl SandboxCopy {
    fn count(&mut self, stat: &std::fs::Metadata) {
        if !stat.is_dir() {
            self.files += 1;
            if stat.is_file() {
                self.bytes += stat.len();
            }
        }
    }
}

/// The name of the entry a journal copy leaves to [`copy_for_sandbox`].
pub(crate) const HOLDING_DIR: &str = DIR;

/// Refuse a journal whose `workbench/` is a link or a file, before a clone
/// writes anything: copying through it would copy what it leads to.
pub(crate) fn refuse_unreal_holding(journal: &Path) -> Result<()> {
    let holding = journal.join(DIR);
    match std::fs::symlink_metadata(&holding) {
        Ok(metadata) if !metadata.is_dir() => bail!(
            "{} {}, not a directory of the journal; `arc journal doctor` reports it",
            holding.display(),
            leads(&holding)
        ),
        _ => Ok(()),
    }
}

/// Copy the workbenches of the journal at `source` into the journal copy at
/// `destination`: every directory under `workbench/` with arc's index, and,
/// when `content` is set, everything else in it. No link is followed: a link
/// is copied as the link it is.
pub(crate) fn copy_for_sandbox(
    source: &Path,
    destination: &Path,
    content: bool,
) -> Result<SandboxCopy> {
    refuse_unreal_holding(source)?;
    let mut copy = SandboxCopy {
        content,
        ..SandboxCopy::default()
    };
    let from = source.join(DIR);
    if !from.is_dir() {
        return Ok(copy);
    }
    let to = destination.join(DIR);
    std::fs::create_dir_all(&to).with_context(|| format!("cannot create {}", to.display()))?;
    for entry in
        std::fs::read_dir(&from).with_context(|| format!("cannot read {}", from.display()))?
    {
        let entry = entry.with_context(|| format!("cannot read {}", from.display()))?;
        let (path, target) = (entry.path(), to.join(entry.file_name()));
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if !metadata.is_dir() {
            // Not a workbench: material standing beside them.
            if content {
                copy_entry(&path, &target, &mut copy)?;
            } else {
                copy.count(&metadata);
            }
            continue;
        }
        copy.directories += 1;
        std::fs::create_dir(&target)
            .with_context(|| format!("cannot create {}", target.display()))?;
        let opened = Dir::open(&path).with_context(|| format!("cannot open {}", path.display()))?;
        if is_arc_index(&opened) {
            std::fs::copy(path.join(INDEX), target.join(INDEX))
                .with_context(|| format!("cannot copy {}", path.join(INDEX).display()))?;
        }
        if !content {
            let left = walk(&opened, &path, None, |_, _, _, _| Ok(()))?;
            copy.files += left.files;
            copy.bytes += left.bytes;
            continue;
        }
        for inner in
            std::fs::read_dir(&path).with_context(|| format!("cannot read {}", path.display()))?
        {
            let inner = inner.with_context(|| format!("cannot read {}", path.display()))?;
            if inner.file_name() == INDEX && target.join(INDEX).exists() {
                continue;
            }
            copy_entry(&inner.path(), &target.join(inner.file_name()), &mut copy)?;
        }
    }
    Ok(copy)
}

/// Copy `from` to `to` without following a link, counting what it copies:
/// a directory with everything in it, a file with its bytes and mode, a link
/// as the same link. Anything else is counted and not copied.
fn copy_entry(from: &Path, to: &Path, copy: &mut SandboxCopy) -> Result<()> {
    let metadata = std::fs::symlink_metadata(from)
        .with_context(|| format!("cannot read {}", from.display()))?;
    copy.count(&metadata);
    let kind = metadata.file_type();
    let copied = if kind.is_dir() {
        std::fs::create_dir(to).and_then(|()| {
            for entry in std::fs::read_dir(from)? {
                let entry = entry?;
                copy_entry(&entry.path(), &to.join(entry.file_name()), copy)
                    .map_err(std::io::Error::other)?;
            }
            Ok(())
        })
    } else if kind.is_symlink() {
        std::fs::read_link(from).and_then(|target| std::os::unix::fs::symlink(target, to))
    } else if kind.is_file() {
        std::fs::copy(from, to).map(|_| ())
    } else {
        Ok(())
    };
    copied.with_context(|| format!("cannot copy {} to {}", from.display(), to.display()))
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
    let cold = archive_dir(dir);
    let mut holdings = Vec::new();
    for (root, is_cold) in [(dir, false), (cold.as_path(), true)] {
        if is_cold {
            match std::fs::symlink_metadata(root) {
                Ok(metadata) if !metadata.file_type().is_dir() => {
                    problems.push(DoctorFinding {
                        code: "workbench-outside-journal",
                        detail: format!(
                            "{}: {}, not a directory of the journal; arc moves nothing through it",
                            root.display(),
                            leads(root)
                        ),
                    });
                    continue;
                }
                _ => {}
            }
        }
        let holding = root.join(DIR);
        match std::fs::symlink_metadata(&holding) {
            Ok(metadata) if metadata.file_type().is_dir() => holdings.push((holding, is_cold)),
            Ok(_) => problems.push(DoctorFinding {
                code: "workbench-outside-journal",
                detail: format!(
                    "{}: {}, not a directory of the journal; arc moves nothing through it",
                    holding.display(),
                    leads(&holding)
                ),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", holding.display()))
            }
        }
    }
    for workbench in &recorded {
        let (path, storage) = located(dir, workbench);
        if storage == "missing" {
            let linked = [
                hot_path(dir, &workbench.name),
                cold_path(dir, &workbench.name),
            ]
            .into_iter()
            .find(|path| path.is_dir());
            problems.push(match linked {
                Some(linked) => DoctorFinding {
                    code: "workbench-outside-journal",
                    detail: format!(
                        "{}: {} {}, not a directory of the journal",
                        workbench.name,
                        linked.display(),
                        leads(&linked)
                    ),
                },
                None => DoctorFinding {
                    code: "missing-workbench",
                    detail: format!("{}: no directory at {}", workbench.name, path.display()),
                },
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
    for (holding, is_cold) in holdings {
        let mut unrecorded = Vec::new();
        for entry in std::fs::read_dir(&holding)
            .with_context(|| format!("cannot read {}", holding.display()))?
        {
            let entry = entry.with_context(|| format!("cannot read {}", holding.display()))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !names.contains(name.as_str()) {
                unrecorded.push((entry.path(), entry.file_type()?.is_dir()));
            }
        }
        unrecorded.sort();
        for (path, is_dir) in unrecorded {
            let remedy = if !is_cold && is_dir {
                format!(
                    "; adopt it where it stands with `arc journal workbench adopt {} --purpose TEXT`, or remove it",
                    path.display()
                )
            } else {
                String::new()
            };
            problems.push(DoctorFinding {
                code: "unrecorded-workbench",
                detail: format!("{}: no event records it{remedy}", path.display()),
            });
        }
    }
    Ok(())
}

/// What a path that is not a directory of the journal is: a link and where
/// it leads, or something else.
fn leads(path: &Path) -> String {
    match std::fs::read_link(path) {
        Ok(target) => format!("is a link to {}", target.display()),
        Err(_) => "is not a directory".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(name: &str) -> JournalEvent {
        serde_json::from_value(serde_json::json!({
            "schema": "journal-events/2",
            "ts": "2026-01-02T03:04:05Z",
            "topic": "notes",
            "event": "workbench-opened",
            "workbench": name,
            "purpose": "notes",
        }))
        .unwrap()
    }

    fn line(event: &JournalEvent) -> String {
        format!("{}\n", serde_json::to_string(event).unwrap())
    }

    fn prepare(dir: &Path, events: String) {
        let pending = super::super::ExchangeTransaction {
            bodies: Vec::new(),
            body_bases: Some(Vec::new()),
            events,
            events_basis: Some(String::new()),
            receipt: None,
            promotion: None,
        };
        let path = super::super::exchange_pending_path(dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&pending).unwrap()).unwrap();
    }

    /// Claim: after an append error the journal's own files say what
    /// happened: the act's exact record, last for its workbench in the
    /// event log, is recorded, even while a prepared write still holds it;
    /// the same only in a prepared write is prepared; a later record for
    /// the workbench, or none, is absent. Rejects reading an append error
    /// as proof nothing was written, and reporting a published record as
    /// unpublished because the prepared write was read after the log.
    #[test]
    fn the_journal_says_whether_an_act_was_recorded() {
        let event = opened("notes-20260102");
        let mut later = opened("notes-20260102");
        later.event = "workbench-closed".to_string();

        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(outcome(dir.path(), &event), Outcome::Absent));
        std::fs::write(dir.path().join("events.jsonl"), line(&event)).unwrap();
        assert!(matches!(outcome(dir.path(), &event), Outcome::Recorded));
        prepare(dir.path(), line(&event));
        assert!(matches!(outcome(dir.path(), &event), Outcome::Recorded));
        let log = format!("{}{}", line(&event), line(&later));
        std::fs::write(dir.path().join("events.jsonl"), log).unwrap();
        prepare(dir.path(), line(&opened("other-20260102")));
        assert!(matches!(outcome(dir.path(), &event), Outcome::Absent));

        let dir = tempfile::tempdir().unwrap();
        prepare(dir.path(), line(&event));
        assert!(matches!(outcome(dir.path(), &event), Outcome::Prepared(_)));
    }

    /// Claim: arc's rename never replaces what stands at the destination:
    /// it fails and leaves both entries as they were. Rejects a move that
    /// overwrites a file another process made there.
    #[test]
    fn a_rename_never_replaces_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plan.md"), "moving\n").unwrap();
        std::fs::write(dir.path().join("taken.md"), "made meanwhile\n").unwrap();
        let opened = Dir::open(dir.path()).unwrap();
        let error = fsat::rename_new(
            &opened,
            OsStr::new("plan.md"),
            &opened,
            OsStr::new("taken.md"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert_eq!(read("plan.md"), "moving\n");
        assert_eq!(read("taken.md"), "made meanwhile\n");
        fsat::rename_new(
            &opened,
            OsStr::new("plan.md"),
            &opened,
            OsStr::new("new.md"),
        )
        .unwrap();
        assert_eq!(read("new.md"), "moving\n");
    }

    /// Claim: a walk given a limit stops after that many entries and marks
    /// its counts as lower bounds; without one it counts every file, nested
    /// ones included. Rejects an unbounded listing walk of a huge workbench,
    /// and a bounded walk presenting a partial count as the whole.
    #[test]
    fn a_bounded_walk_stops_and_says_its_counts_are_partial() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..4 {
            std::fs::write(dir.path().join(format!("{index}.txt")), "x").unwrap();
        }
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/deep.txt"), "x").unwrap();
        let root = Dir::open(dir.path()).unwrap();
        let measure = |limit| walk(&root, dir.path(), limit, |_, _, _, _| Ok(())).unwrap();
        let whole = measure(None);
        assert_eq!((whole.files, whole.bytes, whole.partial), (5, 5, false));
        let bounded = measure(Some(3));
        assert!(bounded.partial && bounded.files <= 3);
        let exact = measure(Some(6));
        assert_eq!((exact.files, exact.partial), (5, false));
    }
}
