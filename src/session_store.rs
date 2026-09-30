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
    /// A store holds something it could not read; the reason is tapes' own.
    Unreadable(String),
}

enum LookupIssue {
    Missing,
    Ambiguous(String),
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

/// Tapes resolves exact IDs and useful prefixes. Arc attributes an acting
/// session only when the resolved record names the entire claimed ID.
fn resolve_exact(
    backends: &[Box<dyn Backend>],
    session: &str,
) -> Result<agent_tapes_core::ResolvedSession, LookupIssue> {
    if backends.is_empty() {
        return Err(LookupIssue::Missing);
    }
    let resolved = match agent_tapes_core::resolve_session(backends, session) {
        Ok(resolved) => resolved,
        Err(ResolveError::NotFound {
            truncated: false, ..
        }) => return Err(LookupIssue::Missing),
        Err(error @ ResolveError::Ambiguous { .. }) => {
            return Err(LookupIssue::Ambiguous(error.to_string()));
        }
        Err(error) => return Err(LookupIssue::Unreadable(error.to_string())),
    };
    if resolved.session.id != session {
        return Err(LookupIssue::Missing);
    }
    Ok(resolved)
}

/// Read one session's recording through tapes, matched by its exact id, with
/// the bounded read window tapes applies to every recording.
pub fn read_session(harness: &str, session: &str) -> SessionAnswer {
    let backends = harness_backends(harness);
    let resolved = match resolve_exact(&backends, session) {
        Ok(resolved) => resolved,
        Err(LookupIssue::Missing) => return SessionAnswer::NoRecording,
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
    /// No store of the harness holds a recording for the id.
    NoRecording,
    /// Store resolution or transcript reading could not establish identity.
    Unresolved,
}

/// Why a recording arc found names no model it may report.
pub enum ModelUnavailable {
    /// The recording carries no model selection to attribute.
    NotRecorded,
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
            ModelUnavailable::SubagentActivity => {
                "a subagent recording is newer than the session's last turn, so the session's model need not be the acting one"
            }
        }
    }
}

/// Read the newest selection and operator boundary within tapes' source bound.
pub fn session_identity(harness: &str, session: &str) -> SessionIdentity {
    let backends = harness_backends(harness);
    let resolved = match resolve_exact(&backends, session) {
        Ok(resolved) => resolved,
        Err(LookupIssue::Missing) => return SessionIdentity::NoRecording,
        Err(LookupIssue::Ambiguous(_) | LookupIssue::Unreadable(_)) => {
            return SessionIdentity::Unresolved;
        }
    };
    let backend = &backends[resolved.backend_index];
    // Keep every turn in the file reader's 4 MiB window, so a presentation
    // limit cannot hide its newest operator. Paged stores have a turn cap.
    let turns = if matches!(harness, "claude" | "codex" | "pi") {
        usize::MAX
    } else {
        4096
    };
    let transcript = match backend.transcript(&resolved.session, turns) {
        Ok(transcript) => transcript,
        Err(_) => return SessionIdentity::Unresolved,
    };
    if harness == "claude" && subagent_may_be_acting(backend.as_ref(), &resolved.session) {
        return SessionIdentity::Unnamed(ModelUnavailable::SubagentActivity);
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
) -> bool {
    backend.lineage(session).is_ok_and(|lineage| {
        lineage
            .children
            .iter()
            .any(|child| child.resolved && child.completed_at.is_none())
    })
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
