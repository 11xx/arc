//! The evidence a cost view's session figures rest on, written in the open
//! `tapes-attribution-evidence/1` format tapes reads.
//!
//! The view's own division is the evidence: an executor's runs as spans, a
//! participant's own events as events, and a session the ledger gives one
//! piece of work as a binding with no span. Every record is tier `bound`: it
//! is arc's own record of who acted on what. A session the view gives no
//! figure writes nothing, and neither does one whose figure the ledger does
//! not divide among its work.

use super::{
    executor_role, plain_session_id, portable_cost_metadata, Ctx, Ledger, Person, Role, SessionRow,
    WorkKey,
};
use crate::commands::cost_commit;
use crate::gitio;
use agent_tapes_core::attribution_evidence::{MAX_DIRECTORY_FILES, MAX_LINES, MAX_LINE_BYTES};
use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "tapes-attribution-evidence/1";

/// The harness names tapes lists; a session reference names one of them.
const HARNESSES: [&str; 4] = ["claude", "codex", "pi", "opencode"];

type Session = (String, String);

/// How a record covers its session.
enum Mark {
    /// The whole session served the work.
    Whole,
    /// The session's requests in `[from, to)` served the work; a run still
    /// open has no end yet.
    Span {
        from: DateTime<Utc>,
        to: Option<DateTime<Utc>>,
    },
    /// The session acted for the work at an instant.
    At(DateTime<Utc>),
}

struct Record {
    session: Session,
    work: WorkKey,
    role: Option<&'static str>,
    mark: Mark,
}

/// A session launched another, by the earliest dispatch that bound both.
struct Launch {
    parent: Session,
    child: Session,
    at: DateTime<Utc>,
    label: Option<String>,
    work: WorkKey,
    dispatch: String,
}

/// The evidence one read of the view gives, before it names its work.
#[derive(Default)]
pub(super) struct Exported {
    records: Vec<Record>,
    launches: Vec<Launch>,
    sessions: BTreeSet<Session>,
    /// Sessions the view names and gives no figure.
    unfigured: usize,
    /// Sessions with a figure whose work the ledger does not divide.
    undivided: usize,
    /// Dispatches of exported executors that have no recorded end.
    open_runs: BTreeSet<String>,
}

fn named_harness(harness: &str) -> bool {
    HARNESSES.contains(&harness)
}

impl Person {
    /// Its figure is divided by its own events: it has several items it
    /// recorded events on, and none that only events recorded on its behalf
    /// name.
    pub(super) fn divided(&self) -> bool {
        self.spanned.len() > 1 && !self.spanless()
    }
}

/// The evidence behind the figure of each row, and the sessions named
/// without one.
pub(super) fn collect(
    sessions: &[SessionRow],
    ledger: &Ledger,
    outside: &[serde_json::Value],
) -> Exported {
    let mut exported = Exported::default();
    let mut launches: BTreeMap<(Session, Session), Launch> = BTreeMap::new();
    for row in sessions {
        let key = (row.harness.clone(), row.session.clone());
        if row.measured.facts.is_none() {
            exported.unfigured += 1;
            continue;
        }
        if !named_harness(&row.harness) {
            exported.undivided += 1;
            continue;
        }
        let before = exported.records.len();
        match &row.participation {
            None => executor(&mut exported, &mut launches, row, &key),
            Some(_) => {
                if let Some(person) = ledger.people.get(&key) {
                    participant(&mut exported, ledger.complete, person, &key);
                }
            }
        }
        if exported.records.len() == before {
            exported.undivided += 1;
        } else {
            exported.sessions.insert(key);
        }
    }
    exported.unfigured += outside
        .iter()
        .filter(|named| {
            !sessions.iter().any(|row| {
                named["harness"] == row.harness.as_str() && named["session"] == row.session.as_str()
            })
        })
        .count();
    exported.launches = launches.into_values().collect();
    exported
}

fn executor(
    exported: &mut Exported,
    launches: &mut BTreeMap<(Session, Session), Launch>,
    row: &SessionRow,
    key: &Session,
) {
    let divided = row.spans.len() > 1;
    for span in &row.spans {
        let mark = if divided {
            Mark::Span {
                from: span.dispatched_at,
                to: span.ended_at,
            }
        } else {
            Mark::Whole
        };
        exported.records.push(Record {
            session: key.clone(),
            work: span.work.clone(),
            role: Some(executor_role(span.role.as_deref())),
            mark,
        });
        if span.ended_at.is_none() {
            exported.open_runs.insert(span.dispatch_event_id.clone());
        }
        let Some(caller) = span.caller.as_ref().filter(|caller| {
            span.parent.is_none()
                && named_harness(&caller.0)
                && plain_session_id(&caller.1)
                && *caller != key
        }) else {
            continue;
        };
        let launch = Launch {
            parent: caller.clone(),
            child: key.clone(),
            at: span.dispatched_at,
            label: span
                .handle
                .clone()
                .filter(|handle| portable_cost_metadata(handle)),
            work: span.work.clone(),
            dispatch: span.dispatch_event_id.clone(),
        };
        let earlier = launches
            .get(&(caller.clone(), key.clone()))
            .is_some_and(|held| (held.at, &held.dispatch) <= (launch.at, &launch.dispatch));
        if !earlier {
            launches.insert((caller.clone(), key.clone()), launch);
        }
    }
}

fn participant(exported: &mut Exported, complete: bool, person: &Person, key: &Session) {
    if !complete {
        return;
    }
    if person.divided() {
        for (event, role) in &person.events {
            exported.records.push(Record {
                session: key.clone(),
                work: event.work.clone(),
                role: role.map(Role::name),
                mark: Mark::At(event.at),
            });
        }
        return;
    }
    let mut membership = person.membership.iter();
    if let (Some(work), None) = (membership.next(), membership.next()) {
        let mut roles = person.roles.iter();
        let role = match (roles.next(), roles.next()) {
            (Some(role), None) => Some(role.name()),
            _ => None,
        };
        exported.records.push(Record {
            session: key.clone(),
            work: work.clone(),
            role,
            mark: Mark::Whole,
        });
    }
}

#[derive(Serialize)]
struct Header {
    schema: &'static str,
    producer: String,
    written_at: String,
}

#[derive(Serialize, PartialEq, Eq)]
struct SessionRef<'a> {
    harness: &'a str,
    id: &'a str,
}

#[derive(Serialize, Clone, PartialEq, Eq)]
struct Work {
    label: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    commits: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
}

#[derive(Serialize)]
struct Window {
    from: String,
    to: String,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Line<'a> {
    Binding {
        session: SessionRef<'a>,
        work: &'a Work,
        tier: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        role: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        span: Option<Window>,
    },
    Event {
        session: SessionRef<'a>,
        work: &'a Work,
        tier: &'static str,
        at: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        role: Option<&'static str>,
    },
    Lineage {
        parent: SessionRef<'a>,
        child: SessionRef<'a>,
        tier: &'static str,
        at: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        subject: Option<&'a str>,
    },
}

fn instant(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

fn reference(session: &Session) -> SessionRef<'_> {
    SessionRef {
        harness: &session.0,
        id: &session.1,
    }
}

/// The repository's `origin` remote when it names a place other than this
/// machine, without any credential it carries.
fn origin(ctx: &Ctx) -> Option<String> {
    let configured = gitio::git(&ctx.cwd, &["config", "--get", "remote.origin.url"]).ok()?;
    let url = configured.trim();
    if let Some((scheme, rest)) = url.split_once("://") {
        if scheme.is_empty()
            || scheme.eq_ignore_ascii_case("file")
            || !scheme
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
        {
            return None;
        }
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        // A query or fragment can carry a token as a user name can; one in
        // the authority leaves no path.
        let (host, path) = match host.split_once(['?', '#']) {
            Some((host, _)) => (host, ""),
            None => (host, path.split(['?', '#']).next().unwrap_or_default()),
        };
        return (!host.is_empty()).then(|| format!("{scheme}://{host}/{path}"));
    }
    let (host, path) = url.split_once(':')?;
    let remote = host.len() > 1
        && !host.contains(['/', '\\'])
        && !path.contains(['@', '\\'])
        && !path.is_empty();
    remote.then(|| url.to_string())
}

fn full_object_name(name: &str) -> bool {
    matches!(name.len(), 40 | 64) && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The work a record names: the change id or journal artifact as arc names
/// it, the commits the change holds, and the repository.
struct Works {
    own: BTreeMap<String, Vec<String>>,
    repository: Option<String>,
    named: BTreeMap<WorkKey, Work>,
}

impl Works {
    fn of(&mut self, key: &WorkKey) -> &Work {
        if !self.named.contains_key(key) {
            let (label, commits) = match key {
                WorkKey::Change(id) => (id.clone(), self.commits(id)),
                WorkKey::Journal((_, file)) => (file.clone(), Vec::new()),
                WorkKey::Other(label) => (label.clone(), Vec::new()),
            };
            let work = Work {
                label,
                commits,
                repository: self.repository.clone(),
            };
            self.named.insert(key.clone(), work);
        }
        &self.named[key]
    }

    /// The commits the change made itself; none when its state is
    /// unreadable.
    fn commits(&self, change_id: &str) -> Vec<String> {
        let mut commits: Vec<String> = self
            .own
            .get(change_id)
            .into_iter()
            .flatten()
            .filter(|commit| full_object_name(commit))
            .cloned()
            .collect();
        commits.sort();
        commits.dedup();
        commits
    }
}

/// What `render` wrote.
pub(super) struct Written {
    pub text: String,
    pub sessions: usize,
    pub bindings: usize,
    pub events: usize,
    pub lineage: usize,
    pub shared_commits: usize,
    pub unfigured: usize,
    pub undivided: usize,
    pub open_runs: usize,
}

impl Written {
    pub fn summary(&self) -> String {
        format!(
            "sessions: {}\nrecords: {} binding, {} event, {} lineage\ncommits under several labels: {}\nsessions without a figure: {}\nsessions whose work the ledger does not divide: {}\nruns still open: {}\n",
            self.sessions,
            self.bindings,
            self.events,
            self.lineage,
            self.shared_commits,
            self.unfigured,
            self.undivided,
            self.open_runs,
        )
    }
}

/// The export's lines: the header, then lineage by time, parent and child,
/// and then each session's bindings and events by time and work, so two
/// renders of one ledger differ only in `written_at`, which also ends each
/// span of a run still open. [`divide`] decides the files they make.
pub(super) fn render(
    ctx: &Ctx,
    exported: &Exported,
    producer: &str,
    written_at: DateTime<Utc>,
) -> Result<Written> {
    let changes: BTreeSet<String> = exported
        .records
        .iter()
        .map(|record| &record.work)
        .chain(exported.launches.iter().map(|launch| &launch.work))
        .filter_map(|work| match work {
            WorkKey::Change(id) => Some(id.clone()),
            _ => None,
        })
        .collect();
    let mut works = Works {
        own: cost_commit::own_commits(ctx, &changes),
        repository: origin(ctx),
        named: BTreeMap::new(),
    };
    let header = Header {
        schema: SCHEMA,
        producer: producer.to_string(),
        written_at: instant(written_at),
    };
    let mut text = format!("{}\n", serde_json::to_string(&header)?);
    let mut written = Written {
        text: String::new(),
        sessions: exported.sessions.len(),
        bindings: 0,
        events: 0,
        lineage: 0,
        shared_commits: 0,
        unfigured: exported.unfigured,
        undivided: exported.undivided,
        open_runs: exported.open_runs.len(),
    };

    let mut launches: Vec<&Launch> = exported.launches.iter().collect();
    launches.sort_by(|a, b| (a.at, &a.parent, &a.child).cmp(&(b.at, &b.parent, &b.child)));
    for launch in launches {
        let subject = match &launch.work {
            WorkKey::Other(_) => None,
            key => Some(works.of(key).label.clone()),
        };
        let line = Line::Lineage {
            parent: reference(&launch.parent),
            child: reference(&launch.child),
            tier: "bound",
            at: instant(launch.at),
            label: launch.label.as_deref(),
            subject: subject.as_deref(),
        };
        text.push_str(&serde_json::to_string(&line)?);
        text.push('\n');
        written.lineage += 1;
    }

    let mut lines: Vec<(Session, DateTime<Utc>, String, bool, String)> = Vec::new();
    for record in &exported.records {
        let work = works.of(&record.work).clone();
        let (at, event, line) = match &record.mark {
            Mark::Whole => (
                DateTime::<Utc>::MIN_UTC,
                false,
                Line::Binding {
                    session: reference(&record.session),
                    work: &work,
                    tier: "bound",
                    role: record.role,
                    span: None,
                },
            ),
            Mark::Span { from, to } => {
                let to = to.map_or(written_at, |to| to).max(*from);
                (
                    *from,
                    false,
                    Line::Binding {
                        session: reference(&record.session),
                        work: &work,
                        tier: "bound",
                        role: record.role,
                        span: Some(Window {
                            from: instant(*from),
                            to: instant(to),
                        }),
                    },
                )
            }
            Mark::At(at) => (
                *at,
                true,
                Line::Event {
                    session: reference(&record.session),
                    work: &work,
                    tier: "bound",
                    at: instant(*at),
                    role: record.role,
                },
            ),
        };
        lines.push((
            record.session.clone(),
            at,
            work.label.clone(),
            event,
            serde_json::to_string(&line)?,
        ));
    }
    lines.sort();
    lines.dedup();
    for (_, _, _, event, line) in &lines {
        text.push_str(line);
        text.push('\n');
        if *event {
            written.events += 1;
        } else {
            written.bindings += 1;
        }
    }
    let mut labels: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for work in works.named.values() {
        for commit in &work.commits {
            labels.entry(commit).or_default().insert(&work.label);
        }
    }
    written.shared_commits = labels.values().filter(|held| held.len() > 1).count();
    written.text = text;
    Ok(written)
}

/// The files an export makes, in write order. Each is the header followed by
/// at most [`MAX_LINES`] records, so tapes reads every file to its end; the
/// records keep the export's order, and no record is split.
pub(super) struct Division<'a> {
    header: &'a str,
    parts: Vec<&'a str>,
    records: usize,
}

/// A directory's file names are numbered to four digits, so name order is
/// write order for every count tapes reads.
const _: () = assert!(MAX_DIRECTORY_FILES < 10_000);

impl<'a> Division<'a> {
    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// Each file's text: the header, then its records.
    pub fn files(&self) -> impl Iterator<Item = [&'a str; 2]> + '_ {
        self.parts.iter().map(|records| [self.header, *records])
    }

    /// The export as one file, refused when tapes would stop reading that
    /// file before its end.
    pub fn whole(&self) -> Result<[&'a str; 2]> {
        match self.parts.as_slice() {
            [records] => Ok([self.header, *records]),
            _ => bail!(
                "the evidence holds {} records, and tapes reads at most {MAX_LINES} from one \
                 file; write it as a directory of files with --output-dir DIR, or select less \
                 work",
                self.records
            ),
        }
    }
}

/// Divide rendered evidence into the files tapes reads whole. Refused when a
/// line is longer than [`MAX_LINE_BYTES`], which no file can hold whole, or
/// when the files would number more than the [`MAX_DIRECTORY_FILES`] tapes
/// reads from a directory.
pub(super) fn divide(text: &str) -> Result<Division<'_>> {
    let mut division = Division {
        header: "",
        parts: Vec::new(),
        records: 0,
    };
    let (mut start, mut end, mut held) = (0, 0, 0);
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let bytes = line.strip_suffix('\n').unwrap_or(line).len();
        ensure!(
            bytes <= MAX_LINE_BYTES,
            "line {} of the evidence is {bytes} bytes, and tapes reads at most \
             {MAX_LINE_BYTES} of a line",
            index + 1
        );
        if index == 0 {
            division.header = line;
            (start, end) = (line.len(), line.len());
            continue;
        }
        if held == MAX_LINES {
            division.parts.push(&text[start..end]);
            (start, held) = (end, 0);
        }
        end += line.len();
        held += 1;
        division.records += 1;
    }
    division.parts.push(&text[start..end]);
    ensure!(
        division.parts.len() <= MAX_DIRECTORY_FILES,
        "the evidence holds {} records, {} files of at most {MAX_LINES}, and tapes reads at \
         most {MAX_DIRECTORY_FILES} files from a directory; select less work",
        division.records,
        division.parts.len()
    );
    Ok(division)
}

/// The location `dir` names: its links resolved when it exists, as named
/// when it does not.
fn destination(dir: &Path) -> Result<PathBuf> {
    match fs::canonicalize(dir) {
        Ok(found) => Ok(found),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(dir.to_path_buf()),
        Err(error) => Err(error).with_context(|| format!("cannot use {}", dir.display())),
    }
}

/// A directory's parent, `.` for a bare name.
fn parent_of(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// Refuse an output directory that exists and is not an empty directory, or
/// that is absent and cannot be created: a link to nothing, or a file
/// standing where a directory above it would be.
pub(super) fn vacant(dir: &Path) -> Result<()> {
    match fs::metadata(dir) {
        Ok(metadata) => {
            ensure!(
                metadata.is_dir(),
                "--output-dir {} is not a directory; name a new or empty directory",
                dir.display()
            );
            let mut entries = fs::read_dir(dir)
                .with_context(|| format!("cannot read --output-dir {}", dir.display()))?;
            ensure!(
                entries.next().is_none(),
                "--output-dir {} is not empty; name a new or empty directory",
                dir.display()
            );
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            ensure!(
                fs::symlink_metadata(dir).is_err(),
                "--output-dir {} is a link to nothing; name a new or empty directory",
                dir.display()
            );
            let above = dir
                .ancestors()
                .skip(1)
                .map(|ancestor| {
                    if ancestor.as_os_str().is_empty() {
                        Path::new(".")
                    } else {
                        ancestor
                    }
                })
                .find(|ancestor| ancestor.exists());
            if let Some(above) = above {
                ensure!(
                    above.is_dir(),
                    "--output-dir {} cannot be created: {} is not a directory",
                    dir.display(),
                    above.display()
                );
            }
        }
        Err(error) => {
            return Err(error).with_context(|| format!("cannot use --output-dir {}", dir.display()))
        }
    }
    Ok(())
}

/// What a directory write created, removed again unless it is disarmed: the
/// staging directory with everything in it, then each ancestor it created,
/// deepest first, while that ancestor is empty.
struct Created {
    staging: Option<PathBuf>,
    ancestors: Vec<PathBuf>,
}

impl Drop for Created {
    fn drop(&mut self) {
        if let Some(staging) = &self.staging {
            let _ = fs::remove_dir_all(staging);
        }
        for ancestor in &self.ancestors {
            let _ = fs::remove_dir(ancestor);
        }
    }
}

/// Write each file of `division` into `dir` as `evidence-0001.jsonl`,
/// `evidence-0002.jsonl`, and so on, so that name order is write order.
/// `dir` must be absent or an empty directory. The files are written into a
/// directory beside `dir` and moved into place together, so `dir` holds every
/// file or what it held before; a failure removes what the write created and
/// leaves an empty directory it replaced empty.
pub(super) fn write_directory(dir: &Path, division: &Division) -> Result<()> {
    vacant(dir)?;
    let destination = destination(dir)?;
    let replaces = match fs::metadata(&destination) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("cannot use {}", dir.display())),
    };
    let name = destination
        .file_name()
        .with_context(|| format!("--output-dir {} names no directory", dir.display()))?
        .to_string_lossy();
    let parent = parent_of(&destination);
    let mut created = Created {
        staging: None,
        ancestors: parent
            .ancestors()
            .take_while(|ancestor| !ancestor.as_os_str().is_empty() && !ancestor.exists())
            .map(Path::to_path_buf)
            .collect(),
    };
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let staging = parent.join(format!(".{name}.{}.staging", crate::ids::new_identifier()));
    fs::create_dir(&staging).with_context(|| format!("cannot create {}", staging.display()))?;
    created.staging = Some(staging.clone());
    for (index, file) in division.files().enumerate() {
        let path = staging.join(format!("evidence-{:04}.jsonl", index + 1));
        let mut written = fs::File::create_new(&path)
            .with_context(|| format!("cannot create {}", path.display()))?;
        for text in file {
            written
                .write_all(text.as_bytes())
                .with_context(|| format!("cannot write {}", path.display()))?;
        }
        written
            .sync_all()
            .with_context(|| format!("cannot sync {}", path.display()))?;
    }
    if let Some(permissions) = replaces.clone() {
        fs::set_permissions(&staging, permissions)
            .with_context(|| format!("cannot set the mode of {}", staging.display()))?;
    }
    fs::File::open(&staging)
        .and_then(|opened| opened.sync_all())
        .with_context(|| format!("cannot sync {}", staging.display()))?;
    fs::rename(&staging, &destination)
        .with_context(|| format!("cannot move the evidence into {}", dir.display()))?;
    created.staging = None;
    let synced = fs::File::open(parent).and_then(|opened| opened.sync_all());
    if let Err(error) = synced {
        let _ = fs::remove_dir_all(&destination);
        if let Some(permissions) = replaces {
            let _ = fs::create_dir(&destination)
                .and_then(|()| fs::set_permissions(&destination, permissions));
        }
        return Err(error).with_context(|| format!("cannot sync {}", parent.display()));
    }
    created.ancestors.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "{\"schema\":\"tapes-attribution-evidence/1\"}\n";

    fn evidence(records: usize, last: usize) -> String {
        let mut text = String::from(HEADER);
        for _ in 1..records {
            text.push_str("{\"kind\":\"event\"}\n");
        }
        if records > 0 {
            text.push_str(&"x".repeat(last));
            text.push('\n');
        }
        text
    }

    /// The header and `records` distinct records, each naming its position.
    fn numbered(records: usize) -> String {
        let mut text = String::from(HEADER);
        for record in 0..records {
            text.push_str(&format!("{record}\n"));
        }
        text
    }

    // Claim: evidence tapes reads to its end is written to one file, and
    // evidence it would stop reading early is refused, at tapes' own bounds,
    // with a refusal of too many records naming `--output-dir`. Rejects a
    // file one record or one byte past either bound, a refusal of one exactly
    // at it, and a refusal that leaves the reader no way to export the rest.
    #[test]
    fn evidence_past_what_tapes_reads_whole_is_refused() {
        let text = evidence(MAX_LINES, MAX_LINE_BYTES);
        assert_eq!(divide(&text).unwrap().whole().unwrap().concat(), text);
        let records = divide(&evidence(MAX_LINES + 1, 16))
            .unwrap()
            .whole()
            .unwrap_err()
            .to_string();
        assert!(records.contains("10001 records"), "{records}");
        assert!(records.contains("--output-dir"), "{records}");
        let line = divide(&evidence(3, MAX_LINE_BYTES + 1))
            .err()
            .expect("an overlong line");
        assert!(line.to_string().starts_with("line 4 "), "{line}");
    }

    // Claim: a directory export makes one file per `MAX_LINES` records, each
    // opening with the header, whose records concatenate back to the export
    // in order, and refuses what tapes would not read whole: more files than
    // a directory may hold, or a line no file can hold. Rejects a division
    // one record off at the bound, a later file without the header, records
    // reordered or lost between files, and either bound taken one past or one
    // short.
    #[test]
    fn a_directory_export_divides_at_tapes_bounds() {
        for (records, files) in [(0, 1), (1, 1), (MAX_LINES, 1), (MAX_LINES + 1, 2)] {
            let text = numbered(records);
            let division = divide(&text).unwrap();
            assert_eq!(division.len(), files, "{records} records");
            let mut rejoined = String::from(HEADER);
            for (index, [opening, held]) in division.files().enumerate() {
                assert_eq!(opening, HEADER, "file {index} of {records} records");
                assert!(held.lines().count() <= MAX_LINES, "file {index}");
                rejoined.push_str(held);
            }
            assert_eq!(rejoined, text, "{records} records");
        }
        let split = numbered(MAX_LINES + 1);
        let held: Vec<[&str; 2]> = divide(&split).unwrap().files().collect();
        assert_eq!(held[0][1].lines().count(), MAX_LINES);
        assert_eq!(held[1][1], format!("{MAX_LINES}\n"));

        let most = MAX_LINES * MAX_DIRECTORY_FILES;
        let full = format!("{HEADER}{}", "\n".repeat(most));
        assert_eq!(divide(&full).unwrap().len(), MAX_DIRECTORY_FILES);
        let over = format!("{HEADER}{}", "\n".repeat(most + 1));
        let refused = divide(&over).err().expect("one file too many").to_string();
        assert!(
            refused.contains("1001 files") && refused.contains("at most 1000 files"),
            "{refused}"
        );

        let longest = format!("{HEADER}{}\ny\n", "x".repeat(MAX_LINE_BYTES));
        assert_eq!(divide(&longest).unwrap().len(), 1);
        let overlong = format!("{HEADER}0\n{}\n", "x".repeat(MAX_LINE_BYTES + 1));
        let refused = divide(&overlong)
            .err()
            .expect("an overlong line")
            .to_string();
        assert!(refused.starts_with("line 3 "), "{refused}");
    }
}
