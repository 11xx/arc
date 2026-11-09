//! `ARC_TRACE=1`: one `arc-perf/1` record of the work a command did, written
//! to stderr as the process exits.
//!
//! The counters are logical: each counts a call arc makes, such as one ledger
//! file opened or one child process run, never a syscall. A phase's time is
//! exclusive: a phase entered inside another is subtracted from the outer one,
//! so no instant is counted twice on one thread. Phases on concurrent threads
//! are summed, so their total can exceed the elapsed time.
//!
//! The record carries no path, session id or argument value: a command is its
//! subcommand path, a child is its program name and, for Git, its subcommand.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub(crate) const PERF_SCHEMA: &str = "arc-perf/1";

const TRACE_VAR: &str = "ARC_TRACE";

/// How a process ended, as the record names it.
#[derive(Clone, Copy)]
pub(crate) enum Outcome {
    /// The command ran and chose its exit status.
    Exited,
    /// The command failed with an error printed on stderr.
    Error,
    /// The arguments were refused before any command ran.
    Usage,
    /// A watched signal stopped the command.
    Interrupted,
    /// The command panicked.
    Panicked,
}

impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Outcome::Exited => "exited",
            Outcome::Error => "error",
            Outcome::Usage => "usage",
            Outcome::Interrupted => "interrupted",
            Outcome::Panicked => "panicked",
        }
    }
}

#[derive(Default)]
struct Timed {
    calls: u64,
    exclusive: Duration,
}

#[derive(Default)]
struct Trace {
    command: Option<String>,
    counts: BTreeMap<&'static str, u64>,
    namespaces: BTreeMap<&'static str, BTreeMap<&'static str, u64>>,
    cache: BTreeMap<&'static str, (u64, u64)>,
    phases: BTreeMap<&'static str, Timed>,
    children: BTreeMap<String, Timed>,
}

static STARTED: OnceLock<Instant> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
static EMITTED: AtomicBool = AtomicBool::new(false);
static TRACE: OnceLock<Mutex<Trace>> = OnceLock::new();

thread_local! {
    /// Time spent in phases nested inside each phase open on this thread.
    static NESTED: RefCell<Vec<Duration>> = const { RefCell::new(Vec::new()) };
}

/// Start the clock and read `ARC_TRACE` once. Called first thing in `main`.
pub(crate) fn start() {
    STARTED.get_or_init(Instant::now);
    if std::env::var_os(TRACE_VAR).is_some_and(|value| value == "1") {
        ENABLED.store(true, Ordering::Relaxed);
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            default(info);
            if std::thread::current().name() == Some("main") {
                emit(Outcome::Panicked, Some(101));
            }
        }));
    }
}

fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Whether this process records a trace, for a caller whose label for a
/// counter costs something to compute.
pub(crate) fn tracing() -> bool {
    enabled()
}

fn with(record: impl FnOnce(&mut Trace)) {
    if !enabled() {
        return;
    }
    let trace = TRACE.get_or_init(Default::default);
    record(
        &mut trace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
}

/// The subcommand path the arguments selected.
pub(crate) fn command(path: &str) {
    with(|trace| trace.command = Some(path.to_string()));
}

/// Add `by` to a named counter.
pub(crate) fn count(name: &'static str, by: u64) {
    with(|trace| *trace.counts.entry(name).or_default() += by);
}

/// Add `by` to one ledger namespace's counter: `change`, `repository` or
/// `import`.
pub(crate) fn ledger(namespace: &'static str, name: &'static str, by: u64) {
    with(|trace| {
        *trace
            .namespaces
            .entry(namespace)
            .or_default()
            .entry(name)
            .or_default() += by;
    });
}

/// A value read once per process was served from what an earlier read held
/// (`hit`) or read afresh (`miss`).
pub(crate) fn cache(name: &'static str, hit: bool) {
    with(|trace| {
        let held = trace.cache.entry(name).or_default();
        if hit {
            held.0 += 1;
        } else {
            held.1 += 1;
        }
    });
}

/// The ledger namespace a ledger file's path lies in, read from the
/// directories that hold it: `change`, `repository`, `import`, or `other`.
pub(crate) fn namespace_of(path: &std::path::Path) -> &'static str {
    let mut held = path
        .components()
        .rev()
        .skip(1)
        .take(3)
        .map(|part| part.as_os_str().to_string_lossy().into_owned());
    let parent = held.next();
    let grandparent = held.next();
    let above = held.next();
    match (parent.as_deref(), grandparent.as_deref(), above.as_deref()) {
        (Some("events"), Some("repository"), _) => "repository",
        (Some("events"), _, Some("changes")) => "change",
        (Some("imports"), _, _) => "import",
        _ => "other",
    }
}

/// A timed phase, closed when dropped.
pub(crate) struct Phase {
    open: Option<(&'static str, Instant)>,
}

/// Open a phase: `enumerate`, `read`, `decode`, `reduce`, `scope`,
/// `round_trip`, `journal`, `sessions` or `children`.
pub(crate) fn phase(name: &'static str) -> Phase {
    if !enabled() {
        return Phase { open: None };
    }
    NESTED.with(|nested| nested.borrow_mut().push(Duration::ZERO));
    Phase {
        open: Some((name, Instant::now())),
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        let Some((name, started)) = self.open else {
            return;
        };
        let exclusive = close(started.elapsed());
        with(|trace| {
            let timed = trace.phases.entry(name).or_default();
            timed.calls += 1;
            timed.exclusive += exclusive;
        });
    }
}

/// Pop this thread's innermost phase, hand its whole time to the phase
/// around it, and answer the time spent in it alone.
fn close(total: Duration) -> Duration {
    NESTED.with(|nested| {
        let mut nested = nested.borrow_mut();
        let inner = nested.pop().unwrap_or_default();
        if let Some(outer) = nested.last_mut() {
            *outer += total;
        }
        total.saturating_sub(inner)
    })
}

/// The name a child is counted under: its program's file name and, for Git,
/// its subcommand. Option values and every other argument are left out.
fn child_kind(command: &std::process::Command) -> String {
    let program = std::path::Path::new(command.get_program())
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if program != "git" {
        return program;
    }
    let mut args = command.get_args().map(|arg| arg.to_string_lossy());
    while let Some(arg) = args.next() {
        if arg == "-C" || arg == "-c" {
            args.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        if arg
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return format!("git {arg}");
        }
        break;
    }
    program
}

/// Run a child to completion through `run`, counting it and its time.
pub(crate) fn child<T>(
    command: &mut std::process::Command,
    run: impl FnOnce(&mut std::process::Command) -> T,
) -> T {
    if !enabled() {
        return run(command);
    }
    let kind = child_kind(command);
    let phase = phase("children");
    let started = Instant::now();
    let answer = run(command);
    let elapsed = started.elapsed();
    drop(phase);
    with(|trace| {
        let timed = trace.children.entry(kind).or_default();
        timed.calls += 1;
        timed.exclusive += elapsed;
    });
    answer
}

/// Count a child arc starts and does not wait for where it starts it; the
/// wait, if any, is the caller's.
pub(crate) fn spawned(command: &std::process::Command) {
    if !enabled() {
        return;
    }
    let kind = child_kind(command);
    with(|trace| trace.children.entry(kind).or_default().calls += 1);
}

/// Run a command to completion, counted as a child.
pub(crate) trait Counted {
    /// The command's output, bounded by the thread's deadline scope; see
    /// `gitio::bounded_output`.
    fn counted_output(&mut self) -> std::io::Result<std::process::Output>;
    fn counted_spawn(&mut self) -> std::io::Result<std::process::Child>;
}

impl Counted for std::process::Command {
    fn counted_output(&mut self) -> std::io::Result<std::process::Output> {
        child(self, crate::gitio::bounded_output)
    }

    fn counted_spawn(&mut self) -> std::io::Result<std::process::Child> {
        spawned(self);
        self.spawn()
    }
}

fn millis(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1_000_000.0).round() / 1_000.0
}

fn record(outcome: Outcome, exit_code: Option<i32>) -> serde_json::Value {
    let trace = TRACE.get_or_init(Default::default);
    let trace = trace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let phases: serde_json::Map<_, _> = trace
        .phases
        .iter()
        .map(|(name, timed)| {
            (
                (*name).to_string(),
                serde_json::json!({"calls": timed.calls, "ms": millis(timed.exclusive)}),
            )
        })
        .collect();
    let children: serde_json::Map<_, _> = trace
        .children
        .iter()
        .map(|(kind, timed)| {
            (
                kind.clone(),
                serde_json::json!({"runs": timed.calls, "ms": millis(timed.exclusive)}),
            )
        })
        .collect();
    let cache: serde_json::Map<_, _> = trace
        .cache
        .iter()
        .map(|(name, (hits, misses))| {
            (
                (*name).to_string(),
                serde_json::json!({"hits": hits, "misses": misses}),
            )
        })
        .collect();
    serde_json::json!({
        "schema": PERF_SCHEMA,
        "command": trace.command,
        "build": {
            "commit": env!("ARC_BUILD_COMMIT"),
            "tree": env!("ARC_BUILD_DIRTY"),
        },
        "outcome": outcome.name(),
        "exit_code": exit_code,
        "elapsed_ms": millis(STARTED.get().map_or(Duration::ZERO, Instant::elapsed)),
        "phases": phases,
        "counts": trace.counts,
        "ledger": trace.namespaces,
        "cache": cache,
        "children": {
            "runs": trace.children.values().map(|timed| timed.calls).sum::<u64>(),
            "by_kind": children,
        },
        "coverage": {
            "ledger": "store",
            "children": "arc",
        },
    })
}

/// Write the record once, as one line on stderr, when tracing is on.
pub(crate) fn emit(outcome: Outcome, exit_code: Option<i32>) {
    if !enabled() || EMITTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut line = record(outcome, exit_code).to_string();
    line.push('\n');
    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(line.as_bytes());
    let _ = stderr.flush();
}
