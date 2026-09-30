//! The relation ledger: read records, declarations, and capture reports,
//! replayed from events and checked on the way in.
//!
//! A relation attaches a piece of context to a subject, a change or a
//! candidate, and carries how it is established: a read record is a tool's
//! record of returned bytes, a declaration is its declarant's claim, and a
//! capture report is a claim about whether the recording is retained. The
//! rules here need no local objects, so a bundle's relations are judged by
//! exactly the rules a local write is; the checks that read the object store,
//! the journal, or a change's claims live with the command that records.

use crate::model::{
    CaptureState, DeclaredRelation, DeclaredTarget, Event, InferredBlob, Payload, ReadArtifact,
    ReadCoverage, RelationSubject,
};
use chrono::{DateTime, Utc};
use std::fmt;

#[derive(Debug, Clone)]
pub struct Read {
    pub subject: RelationSubject,
    pub episode: String,
    pub record: String,
    pub path: String,
    pub digest: String,
    pub coverage: ReadCoverage,
    pub compared_at: Option<String>,
    pub blob: Option<InferredBlob>,
    pub artifact: Option<ReadArtifact>,
    pub source: Option<String>,
    pub event_id: String,
    pub declarant: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Declaration {
    pub subject: RelationSubject,
    pub relation: DeclaredRelation,
    pub target: DeclaredTarget,
    pub citation: Option<String>,
    pub event_id: String,
    pub declarant: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Capture {
    pub subject: RelationSubject,
    pub record: String,
    pub capture: CaptureState,
    pub event_id: String,
    pub declarant: String,
}

/// A write the ledger refuses. Each carries its stable code first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Malformed {
        event_id: String,
        detail: String,
    },
    DuplicateRecord {
        subject: RelationSubject,
        record: String,
    },
    UnknownCitation {
        subject: RelationSubject,
        record: String,
    },
    UnknownRecord {
        subject: RelationSubject,
        record: String,
    },
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::Malformed { .. } => "malformed-relation-event",
            Refusal::DuplicateRecord { .. } => "duplicate-record",
            Refusal::UnknownCitation { .. } => "unknown-citation",
            Refusal::UnknownRecord { .. } => "unknown-record",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.code())?;
        match self {
            Refusal::Malformed { event_id, detail } => write!(f, "event {event_id}: {detail}"),
            Refusal::DuplicateRecord { subject, record } => write!(
                f,
                "tool record {record} is already recorded as a read on {}",
                subject.label()
            ),
            Refusal::UnknownCitation { subject, record } => write!(
                f,
                "citation {record} resolves to no read recorded on {}; record the read first",
                subject.label()
            ),
            Refusal::UnknownRecord { subject, record } => write!(
                f,
                "tool record {record} is not recorded as a read on {}",
                subject.label()
            ),
        }
    }
}

impl std::error::Error for Refusal {}

/// Every relation in a set of events, for every subject they name.
#[derive(Debug, Default)]
pub struct Relations {
    reads: Vec<Read>,
    declarations: Vec<Declaration>,
    captures: Vec<Capture>,
}

impl Relations {
    /// Replay relation events in order, refusing at the first one the ledger
    /// would not have recorded. Events of other kinds are skipped.
    pub fn replay<'a>(events: impl IntoIterator<Item = &'a Event>) -> Result<Relations, Refusal> {
        let mut relations = Relations::default();
        for event in events {
            relations.record(event)?;
        }
        Ok(relations)
    }

    /// Check one event against the ledger and apply it.
    pub fn record(&mut self, event: &Event) -> Result<(), Refusal> {
        let declarant = event
            .on_behalf_of
            .clone()
            .unwrap_or_else(|| event.actor.clone());
        let malformed = |detail: String| Refusal::Malformed {
            event_id: event.event_id.clone(),
            detail,
        };
        match &event.payload {
            Payload::ContextRead {
                subject,
                episode,
                record,
                path,
                digest,
                coverage,
                compared_at,
                blob,
                artifact,
                source,
            } => {
                for (label, value) in [("episode", episode), ("record", record), ("path", path)] {
                    well_named(label, value).map_err(malformed)?;
                }
                if !is_digest(digest) {
                    return Err(malformed(format!(
                        "digest {digest:?} is not a sha256: digest"
                    )));
                }
                valid_coverage(coverage).map_err(malformed)?;
                if blob.is_some() && compared_at.is_none() {
                    return Err(malformed(
                        "an inferred blob names the revision it was compared at".to_string(),
                    ));
                }
                self.check_read(subject, record)?;
                self.reads.push(Read {
                    subject: subject.clone(),
                    episode: episode.clone(),
                    record: record.clone(),
                    path: path.clone(),
                    digest: digest.clone(),
                    coverage: *coverage,
                    compared_at: compared_at.clone(),
                    blob: blob.clone(),
                    artifact: artifact.clone(),
                    source: source.clone(),
                    event_id: event.event_id.clone(),
                    declarant,
                    recorded_at: event.created_at,
                });
            }
            Payload::ContextDeclared {
                subject,
                relation,
                target,
                citation,
            } => {
                match target {
                    DeclaredTarget::Path { path, .. } => well_named("path", path),
                    DeclaredTarget::Artifact { file } => well_named("artifact", file),
                }
                .map_err(malformed)?;
                if let Some(citation) = citation {
                    self.check_citation(subject, citation)?;
                }
                self.declarations.push(Declaration {
                    subject: subject.clone(),
                    relation: *relation,
                    target: target.clone(),
                    citation: citation.clone(),
                    event_id: event.event_id.clone(),
                    declarant,
                    recorded_at: event.created_at,
                });
            }
            Payload::ContextCaptureReported {
                subject,
                record,
                capture,
            } => {
                self.check_capture(subject, record)?;
                self.captures.push(Capture {
                    subject: subject.clone(),
                    record: record.clone(),
                    capture: *capture,
                    event_id: event.event_id.clone(),
                    declarant,
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// A tool record is one read per subject.
    pub fn check_read(&self, subject: &RelationSubject, record: &str) -> Result<(), Refusal> {
        if self.read(subject, record).is_some() {
            return Err(Refusal::DuplicateRecord {
                subject: subject.clone(),
                record: record.to_string(),
            });
        }
        Ok(())
    }

    /// A citation names a read recorded on the same subject.
    pub fn check_citation(&self, subject: &RelationSubject, record: &str) -> Result<(), Refusal> {
        if self.read(subject, record).is_none() {
            return Err(Refusal::UnknownCitation {
                subject: subject.clone(),
                record: record.to_string(),
            });
        }
        Ok(())
    }

    /// A capture report names a read recorded on the same subject.
    pub fn check_capture(&self, subject: &RelationSubject, record: &str) -> Result<(), Refusal> {
        if self.read(subject, record).is_none() {
            return Err(Refusal::UnknownRecord {
                subject: subject.clone(),
                record: record.to_string(),
            });
        }
        Ok(())
    }

    pub fn read(&self, subject: &RelationSubject, record: &str) -> Option<&Read> {
        self.reads
            .iter()
            .find(|read| &read.subject == subject && read.record == record)
    }

    pub fn reads_of<'a>(&'a self, subject: &'a RelationSubject) -> impl Iterator<Item = &'a Read> {
        self.reads
            .iter()
            .filter(move |read| &read.subject == subject)
    }

    pub fn reads(&self) -> impl Iterator<Item = &Read> {
        self.reads.iter()
    }

    pub fn declarations_of<'a>(
        &'a self,
        subject: &'a RelationSubject,
    ) -> impl Iterator<Item = &'a Declaration> {
        self.declarations
            .iter()
            .filter(move |declaration| &declaration.subject == subject)
    }

    /// The latest capture report for a read's recording. A read with none,
    /// or whose latest is `unpinned`, is at risk.
    pub fn capture_of(&self, read: &Read) -> Option<&Capture> {
        self.captures
            .iter()
            .rev()
            .find(|capture| capture.subject == read.subject && capture.record == read.record)
    }
}

pub fn is_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn well_named(label: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(format!("{label} {value:?} is blank or padded"));
    }
    Ok(())
}

fn valid_coverage(coverage: &ReadCoverage) -> Result<(), String> {
    match coverage {
        ReadCoverage::Lines { from, to } if *from == 0 || to < from => Err(format!(
            "line range {from}-{to} is not a one-based, ascending range"
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, payload: Payload) -> Event {
        Event {
            schema_version: crate::model::SCHEMA_VERSION,
            event_id: id.to_string(),
            repository_id: "repo".to_string(),
            change_id: "change".to_string(),
            actor: "lead".to_string(),
            actor_source: None,
            operator: None,
            on_behalf_of: None,
            model: None,
            model_provenance: Default::default(),
            harness: None,
            session: None,
            session_link: None,
            session_resolution: None,
            created_at: Utc::now(),
            payload,
        }
    }

    fn subject(change: &str) -> RelationSubject {
        RelationSubject::Change {
            change_id: change.to_string(),
        }
    }

    fn read(id: &str, on: &str, record: &str) -> Event {
        event(
            id,
            Payload::ContextRead {
                subject: subject(on),
                episode: "claim".to_string(),
                record: record.to_string(),
                path: "src/lib.rs".to_string(),
                digest: format!("sha256:{}", "a".repeat(64)),
                coverage: ReadCoverage::Unknown,
                compared_at: None,
                blob: None,
                artifact: None,
                source: None,
            },
        )
    }

    #[test]
    fn a_record_is_one_read_per_subject() {
        let mut relations = Relations::replay([&read("01A", "one", "r1")]).unwrap();
        assert_eq!(
            relations
                .record(&read("01B", "one", "r1"))
                .unwrap_err()
                .code(),
            "duplicate-record"
        );
        relations.record(&read("01C", "two", "r1")).unwrap();
    }

    #[test]
    fn a_citation_resolves_only_on_its_own_subject() {
        let relations = Relations::replay([&read("01A", "one", "r1")]).unwrap();
        relations.check_citation(&subject("one"), "r1").unwrap();
        assert_eq!(
            relations
                .check_citation(&subject("two"), "r1")
                .unwrap_err()
                .code(),
            "unknown-citation"
        );
    }

    #[test]
    fn the_latest_capture_report_stands() {
        let capture = |id: &str, state| {
            event(
                id,
                Payload::ContextCaptureReported {
                    subject: subject("one"),
                    record: "r1".to_string(),
                    capture: state,
                },
            )
        };
        let relations = Relations::replay([
            &read("01A", "one", "r1"),
            &capture("01B", CaptureState::Pinned),
            &capture("01C", CaptureState::Unpinned),
        ])
        .unwrap();
        let read = relations.read(&subject("one"), "r1").unwrap();
        assert_eq!(
            relations.capture_of(read).unwrap().capture,
            CaptureState::Unpinned
        );
    }

    #[test]
    fn a_backwards_line_range_is_malformed() {
        let mut bad = read("01A", "one", "r1");
        if let Payload::ContextRead { coverage, .. } = &mut bad.payload {
            *coverage = ReadCoverage::Lines { from: 5, to: 2 };
        }
        assert_eq!(
            Relations::replay([&bad]).unwrap_err().code(),
            "malformed-relation-event"
        );
    }
}
