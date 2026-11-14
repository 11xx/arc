//! `arc workspace watch`: the workspace report, emitted again each time arc
//! records something that changes it.
//!
//! The watch follows the workspace feed (`crate::feed`) through one inotify
//! watch on its directory and sleeps in `poll` between notices, so an idle
//! watch spends no CPU and holds no Git process. Once the feed has been quiet
//! for `SETTLE`, it forgets what the notices name, reads again only the
//! projects they belong to, and emits the report when anything but its
//! observation times changed. Every other project keeps the row and ledger
//! facts its last read gave.

use super::*;
use crate::feed::Feed;
use std::collections::HashMap;
use std::io::Write;
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

/// How long the feed stays quiet before the watch reads, so one command's
/// writes land in one report.
const SETTLE: Duration = Duration::from_millis(250);
/// The longest a notice waits for the feed to go quiet.
const SETTLE_AT_MOST: Duration = Duration::from_secs(2);
/// How many of the projects written last the watch keeps read between
/// notices, ledger and journal. The next write most likely lands in one of
/// them, and a held ledger reads one written change again rather than every
/// change.
const HOT: usize = 2;

const REQUEST_SCHEMA: &str = "arc-workspace-watch-request/1";

pub(super) fn watch(
    ctx: &Ctx,
    scope: WorkspaceScope,
    previous: Option<&Path>,
    show_retired: bool,
    journal: JournalRead,
) -> Result<i32> {
    crate::store::share_reads();
    // The feed is followed from its end before the first read, so a write
    // that lands during that read is a notice the watch reads after it.
    let mut feed = Feed::open()?;
    let mut watch = Watch::new(ctx, scope, show_retired, journal)?;
    if let Some(path) = previous {
        watch.previous = Some(previous_report(path, &watch.scope.view())?);
    }
    let mut out = std::io::stdout();
    watch.read_and_emit(&mut out)?;

    let mut requests = Requests::default();
    let mut pending: Option<(Instant, Instant)> = None;
    loop {
        let timeout = pending.map(|(first, last)| {
            let due = (last + SETTLE).min(first + SETTLE_AT_MOST);
            due.saturating_duration_since(Instant::now())
        });
        let ready = wait(feed.fd(), timeout)?;
        if ready.output_gone {
            return Ok(0);
        }
        if ready.feed {
            feed.drain_wakeups();
            let (lines, lost) = feed.lines();
            for line in lines {
                watch.note(&line);
            }
            // Notices the watch could not read could have named anything.
            watch.dirty.everything |= lost;
            if watch.dirty.any() {
                let now = Instant::now();
                pending = Some(pending.map_or((now, now), |(first, _)| (first, now)));
            }
        }
        if ready.requests {
            let Some(lines) = requests.read() else {
                return Ok(0);
            };
            let mut asked = false;
            for line in lines {
                match watch.request(&line) {
                    Ok(()) => asked = true,
                    Err(error) => eprintln!("arc: workspace watch request refused: {error:#}"),
                }
            }
            // A request is answered at once, with whatever notices are pending.
            if asked {
                pending = None;
                watch.read_or_name_failure(&mut out);
            }
        }
        if let Some((first, last)) = pending {
            let now = Instant::now();
            if now >= last + SETTLE || now >= first + SETTLE_AT_MOST {
                pending = None;
                watch.read_or_name_failure(&mut out);
            }
        }
    }
}

/// What a notice or request asked the watch to read again.
#[derive(Default)]
struct Dirty {
    /// Forget every held read, then read the registry and every project.
    everything: bool,
    /// Read the registry again.
    registry: bool,
    /// Ledgers by canonical root: the changes written, or `None` for a write
    /// that was not one change's event.
    ledgers: HashMap<PathBuf, Option<BTreeSet<String>>>,
    /// Journal directories as registered.
    journals: BTreeSet<PathBuf>,
    /// Build the report again from what is held.
    report: bool,
}

impl Dirty {
    fn any(&self) -> bool {
        self.everything
            || self.registry
            || !self.ledgers.is_empty()
            || !self.journals.is_empty()
            || self.report
    }
}

struct Watch<'a> {
    journal: JournalRead,
    ctx: &'a Ctx,
    scope: ResolvedWorkspaceScope,
    listed: Listed,
    registered: Vec<crate::registry::Project>,
    /// Each registered ledger by canonical root: the root as registered, which
    /// the store's reads are held under, and the journals reading it.
    ledgers: HashMap<PathBuf, (PathBuf, Vec<PathBuf>)>,
    /// Each registered journal directory by its canonical path.
    journals: HashMap<PathBuf, PathBuf>,
    /// Each observed project's row, by journal directory.
    rows: HashMap<PathBuf, ProjectBacklog>,
    /// Each reported project's ledger facts and the failures reading them,
    /// by journal directory.
    facts: HashMap<PathBuf, (Option<report::LedgerFacts>, Vec<CollectionFailure>)>,
    health: HashMap<PathBuf, Vec<crate::git_health::Finding>>,
    previous: Option<serde_json::Value>,
    /// The last report emitted, serialized without its observation.
    emitted: Option<Vec<u8>>,
    /// The journal directories of the projects written last, most recent
    /// first.
    hot: Vec<PathBuf>,
    dirty: Dirty,
}

impl<'a> Watch<'a> {
    fn new(
        ctx: &'a Ctx,
        scope: WorkspaceScope,
        show_retired: bool,
        journal: JournalRead,
    ) -> Result<Self> {
        let mut watch = Watch {
            journal,
            ctx,
            scope: ResolvedWorkspaceScope::resolve(scope)?,
            listed: Listed {
                unreachable: false,
                retired: show_retired,
            },
            registered: Vec::new(),
            ledgers: HashMap::new(),
            journals: HashMap::new(),
            rows: HashMap::new(),
            facts: HashMap::new(),
            health: HashMap::new(),
            previous: None,
            emitted: None,
            hot: Vec::new(),
            dirty: Dirty::default(),
        };
        watch.read_registry()?;
        Ok(watch)
    }

    fn read_registry(&mut self) -> Result<()> {
        let cfg = crate::config::load()?;
        self.registered = crate::registry::projects(&cfg)?;
        self.ledgers.clear();
        self.journals.clear();
        for project in &self.registered {
            let canonical =
                |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            self.journals
                .insert(canonical(&project.journal_dir), project.journal_dir.clone());
            if let Some(root) = &project.ledger {
                self.ledgers
                    .entry(canonical(root))
                    .or_insert_with(|| (root.clone(), Vec::new()))
                    .1
                    .push(project.journal_dir.clone());
            }
        }
        Ok(())
    }

    /// Mark what one feed line names. A line the watch cannot read could
    /// have named anything, so it reads everything again.
    fn note(&mut self, line: &[u8]) {
        let Ok(notice) = serde_json::from_slice::<serde_json::Value>(line) else {
            self.dirty.everything = true;
            return;
        };
        if notice["schema"] != crate::feed::SCHEMA {
            self.dirty.everything = true;
            return;
        }
        match (
            notice["source"].as_str(),
            notice["path"].as_str(),
            notice["dir"].as_str(),
        ) {
            (Some("ledger"), Some(path), _) => {
                // A ledger no registered project reads is not in the report.
                let Some(root) = self.ledger_of(Path::new(path)) else {
                    return;
                };
                let changes = self
                    .dirty
                    .ledgers
                    .entry(root)
                    .or_insert_with(|| Some(BTreeSet::new()));
                match (changes.as_mut(), notice["change"].as_str()) {
                    (Some(changes), Some(change)) => {
                        changes.insert(change.to_string());
                    }
                    (_, None) => *changes = None,
                    (None, Some(_)) => {}
                }
            }
            (Some("journal"), _, Some(dir)) => {
                let canonical = Path::new(dir)
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(dir));
                match self.journals.get(&canonical) {
                    Some(registered) => {
                        self.dirty.journals.insert(registered.clone());
                    }
                    // A journal the registry did not hold is a new registration.
                    None => self.dirty.registry = true,
                }
                if notice["binding"] == true {
                    self.dirty.registry = true;
                }
            }
            _ => self.dirty.everything = true,
        }
    }

    /// The canonical root of the registered ledger holding `path`.
    fn ledger_of(&self, path: &Path) -> Option<PathBuf> {
        let canonical = path
            .canonicalize()
            .or_else(|_| {
                let parent = path.parent().unwrap_or(path).canonicalize()?;
                Ok::<_, std::io::Error>(parent.join(path.file_name().unwrap_or_default()))
            })
            .unwrap_or_else(|_| path.to_path_buf());
        canonical
            .ancestors()
            .find(|ancestor| self.ledgers.contains_key(*ancestor))
            .map(Path::to_path_buf)
    }

    /// Honor one request line.
    fn request(&mut self, line: &[u8]) -> Result<()> {
        let request: serde_json::Value =
            serde_json::from_slice(line).context("a request is one JSON object per line")?;
        let fields = request
            .as_object()
            .context("a request is one JSON object per line")?;
        ensure!(
            request["schema"] == REQUEST_SCHEMA,
            "a request carries \"schema\": \"{REQUEST_SCHEMA}\""
        );
        let asked: Vec<&str> = fields
            .keys()
            .map(String::as_str)
            .filter(|key| *key != "schema")
            .collect();
        match asked.as_slice() {
            ["previous"] if request["previous"].is_null() => {
                self.previous = None;
                self.dirty.report = true;
            }
            ["previous"] => {
                let path = request["previous"]
                    .as_str()
                    .context("\"previous\" is a report file or null")?;
                self.previous = Some(previous_report(Path::new(path), &self.scope.view())?);
                self.dirty.report = true;
            }
            ["refresh"] if request["refresh"] == true => self.dirty.everything = true,
            _ => anyhow::bail!(
                "a request asks for one of \"previous\" (a report file or null) or \"refresh\" (true)"
            ),
        }
        Ok(())
    }

    /// Read what is dirty and emit the report if it changed. A failed read is
    /// named on stderr; the next notice or request reads again.
    fn read_or_name_failure(&mut self, out: &mut impl Write) {
        if let Err(error) = self.read_and_emit(out) {
            eprintln!("arc: workspace watch: {error:#}");
            self.dirty.everything = true;
        }
    }

    fn read_and_emit(&mut self, out: &mut impl Write) -> Result<()> {
        let mut report = self.read()?;
        let observation = report.observation.take();
        let encode = |report: &report::Report| -> Result<Vec<u8>> {
            // The default stream keeps its sorted JSON keys. A library is
            // serialized from its retained rows without an intermediate tree.
            Ok(if self.journal.library {
                serde_json::to_vec(report)?
            } else {
                serde_json::to_vec(&serde_json::to_value(report)?)?
            })
        };
        let bare = encode(&report)?;
        if self.emitted.as_ref() == Some(&bare) {
            return Ok(());
        }
        self.emitted = Some(bare);
        report.observation = observation;
        let mut line = encode(&report)?;
        line.push(b'\n');
        out.write_all(&line)?;
        out.flush()?;
        Ok(())
    }

    /// Forget what is dirty, read it again, and build the report.
    fn read(&mut self) -> Result<report::Report> {
        let started_at = chrono::Utc::now();
        let dirty = std::mem::take(&mut self.dirty);
        if dirty.everything {
            crate::store::forget_shared_reads(None);
            crate::journal::forget_projections();
            crate::gitio::retain_range_paths(|_| false);
            self.rows.clear();
            self.facts.clear();
            self.health.clear();
            self.hot.clear();
        }
        let mut stale: BTreeSet<PathBuf> = BTreeSet::new();
        for (canonical, changes) in dirty.ledgers {
            let Some((root, journals)) = self.ledgers.get(&canonical) else {
                continue;
            };
            match changes {
                Some(changes) => {
                    for change in changes {
                        crate::store::forget_change(root, &change);
                    }
                }
                None => crate::store::forget_ledger(root),
            }
            stale.extend(journals.iter().cloned());
        }
        // A series item linked into another project reads that project's
        // ledger, so a write there leaves every row linking into it stale.
        let written: BTreeSet<&str> = self
            .registered
            .iter()
            .filter(|project| stale.contains(&project.journal_dir))
            .map(|project| project.slug.as_str())
            .collect();
        let linking: Vec<PathBuf> = self
            .rows
            .iter()
            .filter(|(_, row)| {
                row.progress
                    .as_ref()
                    .is_some_and(|series| super::super::progress::links_into(series, &written))
            })
            .map(|(dir, _)| dir.clone())
            .collect();
        stale.extend(linking);
        for dir in dirty.journals {
            crate::journal::forget_projection(&dir);
            stale.insert(dir);
        }
        if !dirty.everything {
            for dir in &stale {
                self.hot.retain(|held| held != dir);
                self.hot.insert(0, dir.clone());
            }
            self.hot.truncate(HOT);
        }
        if dirty.everything || dirty.registry {
            self.read_registry()?;
        }
        for dir in &stale {
            self.rows.remove(dir);
            self.facts.remove(dir);
        }

        let unread = observe_rows(
            self.ctx,
            &self.registered,
            |project| {
                observed_in(&self.scope, project) && !self.rows.contains_key(&project.journal_dir)
            },
            None,
            self.journal,
        );
        for (index, row) in unread {
            self.rows
                .insert(self.registered[index].journal_dir.clone(), row);
        }
        let mut rows = std::mem::take(&mut self.rows);
        let ctx = self.ctx;
        let mut collected = assemble_backlog(
            std::mem::take(&mut self.registered),
            self.scope.clone(),
            BacklogShape {
                started_at,
                cutoff: None,
                listed: self.listed,
                rank_by: RankBasis::Blocking,
                include_empty: true,
            },
            |_, project, anchor| {
                rows.remove(&project.journal_dir)
                    .unwrap_or_else(|| project_row(ctx, project, anchor, None, self.journal))
            },
        );
        let facts = &mut self.facts;
        let (ledgers, failures) = report_ledger_facts(&collected, |project, observed| {
            facts
                .entry(project.journal_dir.clone())
                .or_insert_with(|| project_ledger_facts(project, observed))
                .clone()
        });
        let reported: BTreeSet<PathBuf> = collected
            .backlog
            .projects
            .iter()
            .map(|row| PathBuf::from(&row.journal_dir))
            .collect();
        self.facts.retain(|dir, _| reported.contains(dir));
        let report = build_report(
            self.ctx,
            &mut collected,
            &ledgers,
            failures,
            self.previous.as_ref(),
            &mut self.health,
        );
        self.rows = collected
            .backlog
            .projects
            .drain(..)
            .map(|row| (PathBuf::from(&row.journal_dir), row))
            .collect();
        self.registered = collected.registered;
        self.shed();
        report
    }

    /// Let go of what the next read most likely does not need: every ledger
    /// and journal read but the hot ones, event bytes, and each `cat-file`
    /// reader. Branches move without a notice, so no Git answer outlives the
    /// read that asked for it. Rows and facts stay.
    fn shed(&self) {
        let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let hot: Vec<&crate::registry::Project> = self
            .registered
            .iter()
            .filter(|project| self.hot.contains(&project.journal_dir))
            .collect();
        let ledgers: Vec<PathBuf> = hot
            .iter()
            .filter_map(|project| project.ledger.as_deref().map(canonical))
            .collect();
        let journals: Vec<PathBuf> = hot
            .iter()
            .map(|project| canonical(&project.journal_dir))
            .collect();
        let checkouts: Vec<&Path> = hot
            .iter()
            .flat_map(|project| [project.ledger.as_deref(), project.anchor.as_deref()])
            .flatten()
            .collect();
        crate::store::retain_shared_reads(|root| ledgers.iter().any(|hot| hot == root));
        crate::journal::retain_projections(|dir| journals.iter().any(|hot| hot == dir));
        crate::gitio::retain_range_paths(|cwd| checkouts.contains(&cwd));
        crate::store::shed_events();
        crate::gitio::forget_answers();
        crate::gitio::close_object_readers();
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        // SAFETY: `malloc_trim` only returns free heap pages to the system.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

/// Standard input, read as request lines.
#[derive(Default)]
struct Requests {
    /// The start of a line whose end has not arrived yet.
    carry: Vec<u8>,
}

impl Requests {
    /// The whole lines that arrived, or `None` at the end of input.
    fn read(&mut self) -> Option<Vec<Vec<u8>>> {
        let mut buffer = [0u8; 4096];
        // SAFETY: the buffer is valid for its length, and the descriptor is
        // the process's standard input, which poll reported readable.
        let count =
            unsafe { libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr().cast(), buffer.len()) };
        let count = usize::try_from(count).ok().filter(|count| *count > 0)?;
        self.carry.extend_from_slice(&buffer[..count]);
        let end = self.carry.iter().rposition(|byte| *byte == b'\n');
        let Some(end) = end else {
            return Some(Vec::new());
        };
        let rest = self.carry.split_off(end + 1);
        let complete = std::mem::replace(&mut self.carry, rest);
        Some(
            complete
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
                .map(<[u8]>::to_vec)
                .collect(),
        )
    }
}

struct Ready {
    feed: bool,
    requests: bool,
    output_gone: bool,
}

/// Sleep until the feed moves, a request arrives, the output loses its
/// reader, or `timeout` passes.
fn wait(feed: RawFd, timeout: Option<Duration>) -> Result<Ready> {
    let mut fds = [
        libc::pollfd {
            fd: feed,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        },
        // Asking for no events still reports an error or hangup: a pipe
        // whose reader closed.
        libc::pollfd {
            fd: libc::STDOUT_FILENO,
            events: 0,
            revents: 0,
        },
    ];
    let timeout = timeout.map_or(-1, |timeout| {
        libc::c_int::try_from(timeout.as_millis().max(1)).unwrap_or(libc::c_int::MAX)
    });
    loop {
        // SAFETY: `fds` is valid for its length for the call.
        let count = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if count >= 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error).context("cannot wait on the workspace feed");
        }
    }
    let [feed, requests, output] = fds;
    let hung_up = libc::POLLERR | libc::POLLHUP;
    Ok(Ready {
        feed: feed.revents & libc::POLLIN != 0,
        // A closed input reads as its end, which `Requests::read` reports.
        requests: requests.revents & (libc::POLLIN | hung_up | libc::POLLNVAL) != 0,
        output_gone: output.revents & hung_up != 0,
    })
}
