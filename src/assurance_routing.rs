//! Routing's answer to where recorded reads stand against the review frontier:
//! `agent-routing frontier --json`, schema `agent-routing-frontier/1`.
//!
//! Arc asks at read time and keeps nothing. A tier is read to place a read
//! against the frontier and to show the gap; no tier, family or price is
//! written anywhere. An answer arc cannot obtain is a fixed reason, never a
//! guess, and never raw output: routing's own error text and any path in it
//! stay out of what arc prints.

use crate::model::Independence;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const FRONTIER_SCHEMA: &str = "agent-routing-frontier/1";
const REFUSAL_SCHEMA: &str = "agent-routing-refusal/1";
const DEADLINE: Duration = Duration::from_secs(30);

/// Why no answer could be had, as a fixed token.
pub type Reason = &'static str;

pub const ROUTING_UNAVAILABLE: Reason = "routing-unavailable";
pub const ROUTING_REFUSED: Reason = "routing-refused";
pub const ROUTING_MALFORMED: Reason = "routing-malformed";

/// Where a read stands against the frontier, as routing placed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Below,
    At,
    Above,
    Unranked,
}

/// One recorded read as routing assessed it under its current route version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedRead {
    pub eligible: bool,
    pub placement: Placement,
    /// The read's own tier; displayed to size a gap, never kept.
    pub tier: Option<i64>,
}

/// Routing's answer for one author and the reads asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// `frontier`, `floor-unknown` or `no-eligible-seat`.
    pub status: String,
    pub route_version: Option<String>,
    pub frontier_tier: Option<i64>,
    /// In the order asked. `None` is a read routing refused to assess.
    pub reads: Vec<Option<RoutedRead>>,
}

#[derive(Deserialize)]
struct RawFrontier {
    schema: String,
    status: String,
    #[serde(default)]
    route_version: Option<String>,
    #[serde(default)]
    frontier: Option<RawSeats>,
    #[serde(default)]
    reads: Vec<RawRead>,
}

#[derive(Deserialize)]
struct RawSeats {
    #[serde(default)]
    review_tier: Option<i64>,
}

#[derive(Deserialize)]
struct RawRead {
    standing: String,
    assessment: RawAssessment,
}

#[derive(Deserialize)]
struct RawAssessment {
    independence: Independence,
    #[serde(default)]
    reviewer: Option<RawReviewer>,
}

#[derive(Deserialize)]
struct RawReviewer {
    #[serde(default)]
    review_tier: Option<i64>,
}

#[derive(Deserialize)]
struct RawRefusal {
    schema: String,
    #[serde(default)]
    role: Option<String>,
}

enum Refused {
    /// The author coordinate was refused, so no read can be placed.
    Author,
    /// A reviewer coordinate was refused.
    Reviewer,
    Other,
}

fn parse(text: &str, asked: usize) -> Result<Answer, Reason> {
    let raw: RawFrontier = serde_json::from_str(text).map_err(|_| ROUTING_MALFORMED)?;
    if raw.schema != FRONTIER_SCHEMA || raw.reads.len() != asked {
        return Err(ROUTING_MALFORMED);
    }
    let reads = raw
        .reads
        .into_iter()
        .map(|read| {
            let placement = match read.standing.as_str() {
                "below" => Placement::Below,
                "at" => Placement::At,
                "above" => Placement::Above,
                "unranked" => Placement::Unranked,
                _ => return Err(ROUTING_MALFORMED),
            };
            Ok(Some(RoutedRead {
                eligible: read.assessment.independence == Independence::Eligible,
                placement,
                tier: read
                    .assessment
                    .reviewer
                    .and_then(|reviewer| reviewer.review_tier),
            }))
        })
        .collect::<Result<Vec<_>, Reason>>()?;
    Ok(Answer {
        status: raw.status,
        route_version: raw.route_version,
        frontier_tier: raw.frontier.and_then(|seats| seats.review_tier),
        reads,
    })
}

/// What a child printed and whether it succeeded, or why it could not run.
/// Routing prints its answer on stdout and a refusal document on stderr.
fn run(author: &str, reads: &[String]) -> Result<Output, Reason> {
    let mut command = Command::new("agent-routing");
    command
        .args(["frontier", "--author", author])
        .args(if reads.is_empty() {
            Vec::new()
        } else {
            std::iter::once("--reads".to_string())
                .chain(reads.iter().cloned())
                .collect()
        })
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().map_err(|_| ROUTING_UNAVAILABLE)?;
    let mut stdout = child.stdout.take().ok_or(ROUTING_UNAVAILABLE)?;
    let mut stderr = child.stderr.take().ok_or(ROUTING_UNAVAILABLE)?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).map(|_| text)
    });
    let diagnostics = std::thread::spawn(move || {
        crate::process_group::read_tail(&mut stderr, crate::process_group::STDERR_TAIL_BYTES)
    });
    let deadline = Instant::now() + DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = crate::process_group::kill_process_group(child.id());
                let _ = child.wait();
                return Err(ROUTING_UNAVAILABLE);
            }
        }
    };
    let text = reader
        .join()
        .map_err(|_| ROUTING_UNAVAILABLE)?
        .map_err(|_| ROUTING_UNAVAILABLE)?;
    let diagnostics = diagnostics
        .join()
        .map_err(|_| ROUTING_UNAVAILABLE)?
        .map_err(|_| ROUTING_UNAVAILABLE)?;
    Ok(Output {
        success: status.success(),
        stdout: text,
        stderr: diagnostics,
    })
}

struct Output {
    success: bool,
    stdout: String,
    stderr: String,
}

/// The refusal document among what a failed call printed, wherever it put it.
fn refusal(output: &Output) -> Refused {
    output
        .stdout
        .lines()
        .chain(output.stderr.lines())
        .find_map(|line| {
            let refusal = serde_json::from_str::<RawRefusal>(line).ok()?;
            (refusal.schema == REFUSAL_SCHEMA).then_some(refusal)
        })
        .map_or(Refused::Other, |refusal| match refusal.role.as_deref() {
            Some("author") => Refused::Author,
            Some("reviewer") => Refused::Reviewer,
            _ => Refused::Other,
        })
}

/// One spawn for `author` and `reads`. A refused reviewer coordinate fails the
/// whole call in routing, so the caller falls back to one read at a time.
fn ask_once(author: &str, reads: &[String]) -> Result<Answer, Refused> {
    match run(author, reads) {
        Ok(output) if output.success => {
            parse(&output.stdout, reads.len()).map_err(|_| Refused::Other)
        }
        Ok(output) => Err(refusal(&output)),
        Err(_) => Err(Refused::Other),
    }
}

/// Memoized questions to routing: each distinct `(author, reads)` set spawns
/// routing once however many changes share it.
#[derive(Default)]
pub struct Routing {
    answers: HashMap<(String, Vec<String>), Result<Answer, Reason>>,
}

impl Routing {
    /// Routing's placement of `reads` (each `model#effort`) for `author`.
    pub fn frontier(&mut self, author: &str, reads: &[String]) -> Result<Answer, Reason> {
        let key = (author.to_string(), reads.to_vec());
        if let Some(known) = self.answers.get(&key) {
            return known.clone();
        }
        let answer = self.ask(author, reads);
        self.answers.insert(key, answer.clone());
        answer
    }

    fn ask(&mut self, author: &str, reads: &[String]) -> Result<Answer, Reason> {
        match ask_once(author, reads) {
            Ok(answer) => Ok(answer),
            Err(Refused::Other) => Err(ROUTING_UNAVAILABLE),
            Err(Refused::Author) => Err(ROUTING_REFUSED),
            Err(Refused::Reviewer) if reads.len() > 1 => {
                // One reviewer routing does not know must not hide the rest.
                let mut merged = None::<Answer>;
                for read in reads {
                    let one = self.frontier(author, std::slice::from_ref(read))?;
                    let slot = one.reads.first().cloned().flatten();
                    match merged.as_mut() {
                        Some(into) => into.reads.push(slot),
                        None => {
                            merged = Some(Answer {
                                reads: vec![slot],
                                ..one
                            });
                        }
                    }
                }
                merged.ok_or(ROUTING_MALFORMED)
            }
            Err(Refused::Reviewer) => {
                // The single read is the one routing refused; its author and
                // frontier are still answerable without it.
                let mut answer = ask_once(author, &[]).map_err(|refused| match refused {
                    Refused::Author => ROUTING_REFUSED,
                    _ => ROUTING_UNAVAILABLE,
                })?;
                answer.reads = vec![None];
                Ok(answer)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(reads: &str) -> String {
        format!(
            r#"{{"schema":"agent-routing-frontier/1","status":"frontier","route_version":"r1",
            "frontier":{{"review_tier":3,"seats":[]}},"reads":[{reads}]}}"#
        )
    }

    #[test]
    fn a_read_is_placed_with_its_independence_and_tier() {
        let text = document(
            r#"{"standing":"below","assessment":{"independence":"eligible","qualifiers":[],
            "reviewer":{"review_tier":2}}}"#,
        );
        let answer = parse(&text, 1).unwrap();
        assert_eq!(answer.frontier_tier, Some(3));
        let read = answer.reads[0].as_ref().unwrap();
        assert!(read.eligible);
        assert_eq!(read.placement, Placement::Below);
        assert_eq!(read.tier, Some(2));
    }

    #[test]
    fn an_answer_that_does_not_cover_every_read_is_malformed() {
        assert_eq!(parse(&document(""), 1), Err(ROUTING_MALFORMED));
        assert_eq!(parse("{}", 0), Err(ROUTING_MALFORMED));
        let other = document("").replace("frontier/1", "frontier/2");
        assert_eq!(parse(&other, 0), Err(ROUTING_MALFORMED));
    }
}
