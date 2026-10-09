//! Harness session recordings, read through the tapes library.
//!
//! Where a harness keeps its sessions and what a recording means is tapes'
//! knowledge; arc links it rather than keeping a second model. The library
//! travels with the binary, so reading a recording needs no `tapes` program
//! on the machine. What arc keeps is its own projection of a recording: the
//! exchange turns and the operator's view of them.

use crate::model::{ModelObservation, ModelTurnRelation};
use agent_tapes_core::backend::{self, Backend};
use agent_tapes_core::model::{Role, SourceBound, TurnKind};
use agent_tapes_core::ResolveError;
use serde::Serialize;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const CLAUDE_MODEL_WAIT: Duration = Duration::from_secs(1);
const CLAUDE_MODEL_POLL: Duration = Duration::from_millis(50);
/// The most characters a lookup reason keeps, its elision marker included.
const REASON_LIMIT: usize = 240;

#[derive(Clone, Debug, Serialize)]
pub struct Turn {
    pub role: String,
    pub text: String,
    pub ts: Option<String>,
}

/// A bound the read rested on, as tapes reports it.
pub enum ReadBound {
    /// Only the final `bytes` of the recording file were read, leaving
    /// `skipped` earlier bytes outside the window.
    FileTail { bytes: u64, skipped: u64 },
    /// The store read stopped at the newest `records` records of one kind.
    RecordPage { records: usize, of: String },
    /// These `turns` carry text the source stored cut at `chars` characters.
    TurnText { turns: usize, chars: usize },
    /// A bound this projection does not name.
    Other,
}

impl ReadBound {
    /// Whether the bound withheld turns rather than shortening text inside a
    /// returned turn. A withheld turn is text outside the read; a text cut is
    /// text inside it.
    pub fn withholds_turns(&self) -> bool {
        !matches!(self, ReadBound::TurnText { .. })
    }
}

/// One recording's exchange turns and what bounded the read.
pub struct SessionRead {
    pub turns: Vec<Turn>,
    pub bound: Option<ReadBound>,
    /// Where the recording lives, when its store is a file.
    pub path: Option<PathBuf>,
}

/// How the store answered for one session.
pub enum SessionAnswer {
    Read(SessionRead),
    /// No store of the harness holds a recording for the id.
    NoRecording,
    /// More than one recording matches the claimed id.
    Ambiguous(String),
    /// The store could not establish or read a recording for the exact id.
    Unreadable(String),
}

enum LookupIssue {
    /// No recording answers for the id, with what was asked.
    Missing(String),
    Ambiguous(String),
    /// No recording could be established for the exact id, with the reason.
    Unreadable(String),
}

/// A lookup reason as one line an operator and an event can carry: control
/// characters become spaces and whitespace runs collapse. Text past
/// `REASON_LIMIT` characters loses its middle to an elision marker, because
/// an error chain names what was asked first and the root cause last.
fn bounded_reason(text: &str) -> String {
    let line = text
        .split(char::is_control)
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ");
    let length = line.chars().count();
    if length <= REASON_LIMIT {
        return line;
    }
    let head = (REASON_LIMIT - 1) / 2;
    let tail = REASON_LIMIT - 1 - head;
    let mut kept: String = line.chars().take(head).collect();
    kept.push('…');
    kept.extend(line.chars().skip(length - tail));
    kept
}

/// The backends of one harness. Asking only the harness a session belongs to
/// keeps a lookup from reaching stores, or programs, of harnesses it does not.
fn harness_backends(harness: &str) -> Vec<Box<dyn Backend>> {
    backend::backends()
        .into_iter()
        .filter(|backend| backend.harness() == harness)
        .collect()
}

/// Tapes resolves exact IDs and useful prefixes. Arc attributes an acting
/// session only when the resolved record names the entire claimed ID; a
/// record under another id is a recording arc cannot attribute, not an
/// absent one.
fn resolve_exact(
    harness: &str,
    backends: &[Box<dyn Backend>],
    session: &str,
) -> Result<agent_tapes_core::ResolvedSession, LookupIssue> {
    if backends.is_empty() {
        return Err(LookupIssue::Missing(format!(
            "arc reads no session store for the {harness} harness"
        )));
    }
    let resolved = match agent_tapes_core::resolve_session(backends, session) {
        Ok(resolved) => resolved,
        Err(ResolveError::NotFound {
            truncated: false, ..
        }) => {
            return Err(LookupIssue::Missing(
                "no recording for this session in the configured store".to_string(),
            ))
        }
        Err(error @ ResolveError::Ambiguous { .. }) => {
            return Err(LookupIssue::Ambiguous(error.to_string()));
        }
        Err(error) => return Err(LookupIssue::Unreadable(error.to_string())),
    };
    if resolved.session.id != session {
        return Err(LookupIssue::Unreadable(format!(
            "session {session} resolves to the recording of a different id, {}",
            resolved.session.id
        )));
    }
    Ok(resolved)
}

/// Read one session's recording through tapes, matched by its exact id, with
/// the bounded read window tapes applies to every recording.
pub fn read_session(harness: &str, session: &str) -> SessionAnswer {
    let backends = harness_backends(harness);
    let resolved = match resolve_exact(harness, &backends, session) {
        Ok(resolved) => resolved,
        Err(LookupIssue::Missing(_)) => return SessionAnswer::NoRecording,
        Err(LookupIssue::Ambiguous(reason)) => return SessionAnswer::Ambiguous(reason),
        Err(LookupIssue::Unreadable(reason)) => return SessionAnswer::Unreadable(reason),
    };
    let backend = &backends[resolved.backend_index];
    let transcript = match backend.transcript(&resolved.session, usize::MAX) {
        Ok(transcript) => transcript,
        Err(error) => return SessionAnswer::Unreadable(format!("{error:#}")),
    };
    let turns = transcript
        .turns
        .into_iter()
        .filter_map(|turn| {
            let role = match turn.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                _ => return None,
            };
            (!turn.text.is_empty()).then(|| Turn {
                role: role.to_string(),
                text: turn.text,
                ts: turn.ts.map(|ts| ts.to_rfc3339()),
            })
        })
        .collect();
    let source_length = transcript.read.as_ref().map(|read| read.source_length);
    let bound = transcript
        .truncation
        .source
        .into_iter()
        .next()
        .map(|bound| match bound {
            SourceBound::FileTail { bytes } => ReadBound::FileTail {
                bytes,
                skipped: source_length.unwrap_or(bytes).saturating_sub(bytes),
            },
            SourceBound::RecordPage { records, of } => ReadBound::RecordPage { records, of },
            SourceBound::TurnText { turns, chars } => ReadBound::TurnText { turns, chars },
            _ => ReadBound::Other,
        });
    let path = transcript
        .session
        .source
        .location
        .as_ref()
        .filter(|location| !location.locator.is_empty())
        .map(|location| PathBuf::from(&location.locator))
        .filter(|path| path.is_file());
    SessionAnswer::Read(SessionRead { turns, bound, path })
}

/// Project session metadata merged from recordings and arc events.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct RecentSession {
    pub(crate) session: String,
    pub(crate) harness: String,
    pub(crate) model: Option<String>,
    pub(crate) last_activity: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) journal_items: usize,
    pub(crate) handoffs: Vec<String>,
    pub(crate) conclusions: Vec<String>,
    pub(crate) unfiled: bool,
}

impl RecentSession {
    pub(crate) fn new(harness: &str, session: &str) -> Self {
        Self {
            session: session.to_string(),
            harness: harness.to_string(),
            model: None,
            last_activity: None,
            journal_items: 0,
            handoffs: Vec::new(),
            conclusions: Vec::new(),
            unfiled: false,
        }
    }

    pub(crate) fn observe(
        &mut self,
        activity: Option<chrono::DateTime<chrono::Utc>>,
        model: Option<&str>,
    ) {
        if activity >= self.last_activity {
            self.last_activity = activity;
            if let Some(model) = model {
                self.model = Some(model.to_string());
            }
        }
    }
}

/// Harness subprocesses inherit sandboxed XDG roots during discovery.
struct DiscoveryEnvironment(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl DiscoveryEnvironment {
    fn sandboxed() -> anyhow::Result<Self> {
        let mut saved = Vec::new();
        if let Some(prefix) = crate::config::sandbox()? {
            for (name, directory) in [
                ("XDG_DATA_HOME", ".local/share"),
                ("XDG_CONFIG_HOME", ".config"),
                ("XDG_CACHE_HOME", ".cache"),
                ("XDG_STATE_HOME", ".local/state"),
            ] {
                saved.push((name, std::env::var_os(name)));
                std::env::set_var(name, prefix.join(directory));
            }
        }
        Ok(Self(saved))
    }
}

impl Drop for DiscoveryEnvironment {
    fn drop(&mut self) {
        for (name, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

/// Discover recordings in the repository's primary checkout and worktrees.
/// Each backend is queried once; every readable row contributes its activity.
pub(crate) fn project_sessions(
    cwd: &std::path::Path,
) -> anyhow::Result<(Vec<RecentSession>, Vec<String>)> {
    let _environment = DiscoveryEnvironment::sandboxed()?;
    let scope = agent_tapes_core::scope::Scope::at(cwd)?;
    let mut query = backend::Query::unscoped(usize::MAX);
    query.scope = Some(&scope);
    let mut sessions = Vec::new();
    let mut unavailable = Vec::new();
    for backend in backend::backends()
        .into_iter()
        .filter(|backend| backend.available())
    {
        match discovery_listing(&mut query, |query| backend.list(query)) {
            Ok(listing) => {
                if listing.scan_truncated {
                    unavailable.push(format!(
                        "{}: incomplete session coverage after {} candidates; ranking may omit sessions outside the backend scan bound",
                        backend.harness(), listing.scanned
                    ));
                }
                unavailable.extend(listing.unavailable);
                for session in listing.sessions {
                    let mut row = RecentSession::new(backend.harness(), &session.id);
                    row.observe(
                        session.last_activity_at,
                        session.model.as_ref().map(model_name).as_deref(),
                    );
                    sessions.push(row);
                }
            }
            Err(error) => unavailable.push(format!("{}: {error:#}", backend.harness())),
        }
    }
    Ok((sessions, unavailable))
}

/// Request every candidate the backend permits in one scan. Backend bounds
/// remain explicit in the listing without withholding readable rows.
fn discovery_listing(
    query: &mut backend::Query<'_>,
    mut list: impl FnMut(&backend::Query<'_>) -> anyhow::Result<backend::Listing>,
) -> anyhow::Result<backend::Listing> {
    query.limit = usize::MAX;
    query.ceiling = usize::MAX;
    list(query)
}

/// What a store holds for one session, and what arc may report from it.
pub enum SessionIdentity {
    /// The store holds a recording naming this model, as `model#effort` when
    /// the recording carries an effort.
    Named {
        model: String,
        observation: Option<ModelObservation>,
    },
    /// The store holds a recording whose model arc will not report, with the
    /// reason.
    Unnamed(ModelUnavailable),
    /// No store of the harness holds a recording for the id, with what was
    /// asked.
    NoRecording(String),
    /// Store resolution, transcript reading, or the subagent lineage read
    /// could not establish identity, with the reason as one bounded line.
    Unresolved(String),
}

/// Why a recording arc found names no model it may report.
pub enum ModelUnavailable {
    /// The recording carries no model selection to attribute.
    NotRecorded,
    /// No model-bearing record appeared during the bounded Claude flush wait.
    NotRecordedAfterWait,
    /// The store holds a subagent recording the session has not recorded as
    /// finished. A subagent's tool shell carries its parent's session id, and
    /// the store does not say which subagent a shell belongs to, so the
    /// parent's model need not be the acting one.
    SubagentActivity,
    OutsideRead,
}

impl ModelUnavailable {
    /// What an operator reads: why no model is named.
    pub fn line(self) -> &'static str {
        match self {
            ModelUnavailable::OutsideRead => "recording head not read; an earlier model selection may be outside the read",
            ModelUnavailable::NotRecorded => "the recording names no model for this session",
            ModelUnavailable::NotRecordedAfterWait => "the recording names no model for this session after waiting up to 1 second for a model-bearing record",
            ModelUnavailable::SubagentActivity => {
                "an unfinished subagent shares this session id; the store does not identify whether this shell belongs to the session or its subagent"
            }
        }
    }
}

/// Read the newest selection and operator boundary within tapes' source bound.
/// A corroborated Claude recording without a model is polled every 50 ms for
/// up to one second to allow the assistant record to reach the store.
pub fn session_identity(harness: &str, session: &str) -> SessionIdentity {
    session_identity_with_backends(harness, session, &harness_backends(harness))
}

fn session_identity_with_backends(
    harness: &str,
    session: &str,
    backends: &[Box<dyn Backend>],
) -> SessionIdentity {
    let resolved = match resolve_exact(harness, backends, session) {
        Ok(resolved) => resolved,
        Err(LookupIssue::Missing(reason)) => {
            return SessionIdentity::NoRecording(bounded_reason(&reason));
        }
        Err(LookupIssue::Ambiguous(reason) | LookupIssue::Unreadable(reason)) => {
            return SessionIdentity::Unresolved(bounded_reason(&reason));
        }
    };
    let backend = backends[resolved.backend_index].as_ref();
    let deadline = Instant::now() + CLAUDE_MODEL_WAIT;
    loop {
        let identity = recording_identity(harness, backend, &resolved.session);
        if harness != "claude"
            || !matches!(
                identity,
                SessionIdentity::Unnamed(ModelUnavailable::NotRecorded)
            )
        {
            return identity;
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return SessionIdentity::Unnamed(ModelUnavailable::NotRecordedAfterWait);
        };
        // Debug test runners publish a model only after this model-less read.
        #[cfg(debug_assertions)]
        if std::env::var_os("ARC_TEST_CLAUDE_MODEL_WAIT_TRACE").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            eprintln!("arc test: Claude model lookup has no model; retrying");
        }
        std::thread::sleep(CLAUDE_MODEL_POLL.min(remaining));
    }
}

fn recording_identity(
    harness: &str,
    backend: &dyn Backend,
    session: &agent_tapes_core::model::Session,
) -> SessionIdentity {
    // Keep every turn in the file reader's 4 MiB window, so a presentation
    // limit cannot hide its newest operator. Paged stores have a turn cap.
    let turns = if matches!(harness, "claude" | "codex" | "pi") {
        usize::MAX
    } else {
        4096
    };
    let transcript = match backend.transcript(session, turns) {
        Ok(transcript) => transcript,
        Err(error) => {
            return SessionIdentity::Unresolved(bounded_reason(&format!(
                "cannot read the recording: {error:#}"
            )))
        }
    };
    if harness == "claude" {
        match subagent_may_be_acting(backend, session) {
            Ok(false) => {}
            Ok(true) => return SessionIdentity::Unnamed(ModelUnavailable::SubagentActivity),
            // An unread lineage cannot rule out the subagent whose shell
            // this may be, so it names no model rather than the parent's.
            Err(error) => {
                return SessionIdentity::Unresolved(bounded_reason(&format!(
                    "cannot read the recording's subagent lineage: {error:#}"
                )))
            }
        }
    }
    let head_read = transcript
        .session
        .model_observation
        .as_ref()
        .is_some_and(|status| status.head_read);
    if let Some(selection) = transcript.session.newest_model_selection() {
        let model = model_name(&selection.model);
        let operator = transcript
            .turns
            .iter()
            .rev()
            .find(|turn| turn.kind == TurnKind::Operator);
        let turn_start = operator.and_then(|turn| turn.ts);
        let turn_relation = match (selection.last.timestamp, turn_start) {
            (Some(record), Some(boundary)) if record >= boundary => ModelTurnRelation::Inside,
            (Some(_), Some(_)) => ModelTurnRelation::Before,
            _ => ModelTurnRelation::Unknown,
        };
        return SessionIdentity::Named {
            model: model.clone(),
            observation: Some(ModelObservation {
                observed: model,
                timestamp: selection.last.timestamp.map(|ts| ts.to_rfc3339()),
                native_id: selection.last.native_id.clone(),
                head_read,
                turn_start: turn_start.map(|ts| ts.to_rfc3339()),
                turn_native_id: operator.and_then(|turn| turn.native_id.clone()),
                turn_relation,
            }),
        };
    }
    if transcript
        .session
        .model_observation
        .as_ref()
        .is_some_and(|status| !status.head_read)
    {
        return SessionIdentity::Unnamed(ModelUnavailable::OutsideRead);
    }
    match transcript.session.model.as_ref() {
        Some(model) => SessionIdentity::Named {
            model: model_name(model),
            observation: None,
        },
        None => SessionIdentity::Unnamed(ModelUnavailable::NotRecorded),
    }
}

fn model_name(model: &agent_tapes_core::model::Model) -> String {
    match &model.variant {
        Some(variant) => format!("{}#{variant}", model.id),
        None => model.id.clone(),
    }
}

/// Whether the store holds a subagent recording the session has not recorded
/// as finished. Such a child may be the process whose shell is asking, and
/// reading its parent's model would answer for the wrong agent.
fn subagent_may_be_acting(
    backend: &dyn Backend,
    session: &agent_tapes_core::model::Session,
) -> anyhow::Result<bool> {
    Ok(backend
        .lineage(session)?
        .children
        .iter()
        .any(|child| child.resolved && child.completed_at.is_none()))
}

/// The operator's view of a transcript: what they asked, plus where the
/// assistant got to.
pub fn operator_view(turns: Vec<Turn>, limit: usize) -> Vec<Turn> {
    let mut users = Vec::new();
    let mut last_message = None;
    for turn in turns {
        if turn.role == "user" {
            users.push(turn.clone());
        }
        last_message = Some(turn);
    }
    if let Some(turn) = last_message.filter(|turn| turn.role == "assistant") {
        users.push(turn);
    }
    let keep_from = users.len().saturating_sub(limit);
    users.drain(keep_from..).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_tapes_core::backend::claude::ClaudeBackend;
    use serde_json::json;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::time::{Duration, Instant};

    #[test]
    fn discovery_queries_capped_backends_once_and_retains_the_listing() {
        let mut query = backend::Query::unscoped(1);
        query.ceiling = 2;
        let mut calls = 0;
        let listing = discovery_listing(&mut query, |window| {
            assert_eq!(window.limit, usize::MAX);
            assert_eq!(window.ceiling, usize::MAX);
            calls += 1;
            Ok(backend::Listing {
                scan_truncated: true,
                scanned: 1_000,
                ..Default::default()
            })
        });
        assert_eq!(calls, 1);
        let listing = listing.unwrap();
        assert_eq!(listing.scanned, 1_000);
        assert!(listing.scan_truncated);
    }

    const SESSION: &str = "11111111-2222-3333-4444-555555555555";

    fn claude_fixture() -> (tempfile::TempDir, PathBuf, Vec<Box<dyn Backend>>) {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("projects/fixture");
        fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{SESSION}.jsonl"));
        let user = json!({"type":"user", "uuid":"u1", "timestamp":"2026-01-01T10:00:00Z",
            "message":{"role":"user", "content":"operator prompt"}});
        fs::write(&path, format!("{user}\n")).unwrap();
        let backend: Box<dyn Backend> = Box::new(ClaudeBackend::new(root.path().join("projects")));
        (root, path, vec![backend])
    }

    #[test]
    fn claude_model_wait_reads_an_assistant_line_flushed_after_lookup() {
        let (_root, path, backends) = claude_fixture();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            let assistant = json!({"type":"assistant", "uuid":"a1", "timestamp":"2026-01-01T10:00:01Z",
                "perTurnEffort":"medium", "message":{"role":"assistant", "model":"fixture-model", "content":"answer"}});
            let mut recording = OpenOptions::new().append(true).open(path).unwrap();
            writeln!(recording, "{assistant}").unwrap();
        });
        let identity = session_identity_with_backends("claude", SESSION, &backends);
        writer.join().unwrap();
        assert!(
            matches!(identity, SessionIdentity::Named { model, observation: Some(_) } if model == "fixture-model#medium")
        );
    }

    #[test]
    fn claude_model_wait_does_not_delay_subagent_ambiguity() {
        let (_root, path, backends) = claude_fixture();
        let children = path.with_extension("").join("subagents");
        fs::create_dir_all(&children).unwrap();
        let assistant = json!({"type":"assistant", "sessionId":SESSION, "uuid":"child-a1",
            "timestamp":"2026-01-01T10:00:01Z", "message":{"role":"assistant", "model":"child-model", "content":"answer"}});
        fs::write(children.join("agent-child.jsonl"), format!("{assistant}\n")).unwrap();
        let start = Instant::now();
        let identity = session_identity_with_backends("claude", SESSION, &backends);
        assert!(matches!(
            identity,
            SessionIdentity::Unnamed(ModelUnavailable::SubagentActivity)
        ));
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn model_wait_does_not_delay_other_harnesses_or_missing_recordings() {
        let (_root, _path, backends) = claude_fixture();
        let start = Instant::now();
        assert!(matches!(
            session_identity_with_backends("codex", SESSION, &backends),
            SessionIdentity::Unnamed(ModelUnavailable::NotRecorded)
        ));
        assert!(matches!(
            session_identity_with_backends("claude", "missing", &backends),
            SessionIdentity::NoRecording(_)
        ));
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    /// A Claude store whose lineage read fails while every other read works.
    struct LineageFails(ClaudeBackend);

    impl Backend for LineageFails {
        fn harness(&self) -> &'static str {
            self.0.harness()
        }
        fn available(&self) -> bool {
            self.0.available()
        }
        fn list(&self, query: &backend::Query) -> anyhow::Result<backend::Listing> {
            self.0.list(query)
        }
        fn locate(&self, id: &str) -> anyhow::Result<Option<agent_tapes_core::model::Session>> {
            self.0.locate(id)
        }
        fn transcript(
            &self,
            session: &agent_tapes_core::model::Session,
            tail: usize,
        ) -> anyhow::Result<agent_tapes_core::model::Transcript> {
            self.0.transcript(session, tail)
        }
        fn kinds(&self) -> agent_tapes_core::model::KindDeclaration {
            self.0.kinds()
        }
        fn lineage(
            &self,
            _session: &agent_tapes_core::model::Session,
        ) -> anyhow::Result<agent_tapes_core::lineage::Lineage> {
            anyhow::bail!("fixture lineage\nfailure")
        }
    }

    fn with_model(path: &std::path::Path) {
        let assistant = json!({"type":"assistant", "uuid":"a1", "timestamp":"2026-01-01T10:00:01Z",
            "message":{"role":"assistant", "model":"parent-model", "content":"answer"}});
        let mut recording = OpenOptions::new().append(true).open(path).unwrap();
        writeln!(recording, "{assistant}").unwrap();
    }

    #[test]
    fn a_failed_lineage_read_is_unresolved_rather_than_no_subagent() {
        let (root, path, _) = claude_fixture();
        with_model(&path);
        let backends: Vec<Box<dyn Backend>> = vec![Box::new(LineageFails(ClaudeBackend::new(
            root.path().join("projects"),
        )))];
        let SessionIdentity::Unresolved(reason) =
            session_identity_with_backends("claude", SESSION, &backends)
        else {
            panic!("a lineage read failure must not name the parent's model");
        };
        assert_eq!(
            reason,
            "cannot read the recording's subagent lineage: fixture lineage failure"
        );
    }

    #[test]
    fn a_prefix_resolution_names_the_claimed_and_resolved_ids() {
        let (_root, path, backends) = claude_fixture();
        with_model(&path);
        let SessionIdentity::Unresolved(reason) =
            session_identity_with_backends("claude", "11111111", &backends)
        else {
            panic!("a prefix of a recorded id is not that recording");
        };
        assert_eq!(
            reason,
            format!("session 11111111 resolves to the recording of a different id, {SESSION}")
        );
    }

    #[test]
    fn a_storeless_harness_is_named_as_such() {
        let SessionIdentity::NoRecording(reason) = session_identity("fixture-harness", SESSION)
        else {
            panic!("no store answers for an unknown harness");
        };
        assert_eq!(
            reason,
            "arc reads no session store for the fixture-harness harness"
        );
    }

    #[test]
    fn reasons_are_one_bounded_line() {
        assert_eq!(bounded_reason("a\nb\tc\u{7}d  e"), "a b c d e");
        let long = format!("asked{}cause", "x".repeat(REASON_LIMIT));
        let bounded = bounded_reason(&long);
        assert_eq!(bounded.chars().count(), REASON_LIMIT);
        assert!(bounded.starts_with("askedx"), "{bounded}");
        assert!(bounded.ends_with("xcause"), "{bounded}");
        assert_eq!(bounded.matches('…').count(), 1, "{bounded}");
        let exact = "y".repeat(REASON_LIMIT);
        assert_eq!(bounded_reason(&exact), exact);
    }

    #[test]
    fn claude_model_wait_gives_up_after_one_second() {
        let (_root, _path, backends) = claude_fixture();
        let start = Instant::now();
        let identity = session_identity_with_backends("claude", SESSION, &backends);
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_secs(1),
            "gave up after {elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(2), "waited {elapsed:?}");
        let SessionIdentity::Unnamed(reason) = identity else {
            panic!("a recording without a model must stay unnamed");
        };
        assert!(reason
            .line()
            .contains("after waiting up to 1 second for a model-bearing record"));
    }
}
