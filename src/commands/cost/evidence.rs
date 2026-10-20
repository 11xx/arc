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
use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

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
            || scheme == "file"
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
struct Works<'a> {
    ctx: &'a Ctx,
    repository: Option<String>,
    named: BTreeMap<WorkKey, Work>,
}

impl Works<'_> {
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

    /// The commits `cost note` finds for the change; none when its state is
    /// unreadable.
    fn commits(&self, change_id: &str) -> Vec<String> {
        let Ok(store) = self.ctx.store() else {
            return Vec::new();
        };
        let Ok(state) = store.read().and_then(|read| read.state(change_id)) else {
            return Vec::new();
        };
        let mut commits: Vec<String> = cost_commit::change_commits(self.ctx, &state)
            .into_iter()
            .filter(|commit| full_object_name(commit))
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
    pub unfigured: usize,
    pub undivided: usize,
    pub open_runs: usize,
}

impl Written {
    pub fn summary(&self) -> String {
        format!(
            "sessions: {}\nrecords: {} binding, {} event, {} lineage\nsessions without a figure: {}\nsessions whose work the ledger does not divide: {}\nruns still open: {}\n",
            self.sessions,
            self.bindings,
            self.events,
            self.lineage,
            self.unfigured,
            self.undivided,
            self.open_runs,
        )
    }
}

/// The file's lines: the header, then lineage by time, parent and child, and
/// then each session's bindings and events by time and work, so two renders
/// of one ledger differ only in `written_at`.
pub(super) fn render(
    ctx: &Ctx,
    exported: &Exported,
    producer: &str,
    written_at: DateTime<Utc>,
) -> Result<Written> {
    let mut works = Works {
        ctx,
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
    written.text = text;
    Ok(written)
}
