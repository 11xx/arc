//! Harness session recordings, read through the tapes library.
//!
//! Where a harness keeps its sessions and what a recording means is tapes'
//! knowledge; arc links it rather than keeping a second model. The library
//! travels with the binary, so reading a recording needs no `tapes` program
//! on the machine. What arc keeps is its own projection of a recording: the
//! exchange turns and the operator's view of them.

use serde::Serialize;
use std::path::PathBuf;
use tapes_core::backend::{self, Backend};
use tapes_core::model::{Role, SourceBound};
use tapes_core::ResolveError;

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
    /// A store holds something it could not read; the reason is tapes' own.
    Unreadable(String),
}

/// The backends of one harness. Asking only the harness a session belongs to
/// keeps a lookup from reaching stores, or programs, of harnesses it does not.
fn harness_backends(harness: &str) -> Vec<Box<dyn Backend>> {
    backend::backends()
        .into_iter()
        .filter(|backend| backend.harness() == harness)
        .collect()
}

/// Read one session's recording through tapes, matched by its exact id, with
/// the bounded read window tapes applies to every recording.
pub fn read_session(harness: &str, session: &str) -> SessionAnswer {
    let backends = harness_backends(harness);
    if backends.is_empty() {
        return SessionAnswer::NoRecording;
    }
    let resolved = match tapes_core::resolve_session(&backends, session) {
        Ok(resolved) => resolved,
        Err(ResolveError::NotFound { .. }) => return SessionAnswer::NoRecording,
        Err(error) => return SessionAnswer::Unreadable(format!("{error}")),
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

/// Whether a harness's store holds the session, and the model and effort its
/// recording shows last, as `model#effort` when both are recorded. The
/// recording's bounded read decides the model; the store's listing answers
/// only when that read does not.
pub fn session_model(harness: &str, session: &str) -> (bool, Option<String>) {
    let backends = harness_backends(harness);
    let Ok(resolved) = tapes_core::resolve_session(&backends, session) else {
        return (false, None);
    };
    let model = backends[resolved.backend_index]
        .transcript(&resolved.session, 1)
        .ok()
        .and_then(|transcript| transcript.session.model)
        .or(resolved.session.model);
    let model = model.map(|model| match model.variant {
        Some(variant) => format!("{}#{variant}", model.id),
        None => model.id,
    });
    (true, model)
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
