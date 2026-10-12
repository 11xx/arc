//! The dated review assurance of integrated changes: every read a change
//! received, and where those reads stand against the strongest review routing
//! can seat now.
//!
//! Nothing here is written. The reads are the verdict and audit events the
//! ledger already holds; staleness is asked of routing each time the view is
//! read, and its components (frontier gap, live fraction, exposure, age) are
//! displayed and never stored. No tier, price or score reaches the ledger.
//! Staleness gates nothing: no readiness, integration or waiver path reads it.

use crate::assurance_live::{self as live, Survival};
use crate::assurance_routing::{self as routing, Answer, Placement, Routing};
use crate::model::{model_effort, Closure, Verdict};
use crate::policy::Danger;
use crate::state::{ChangeState, DebtStanding, Patchset};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "arc-assurance/1";

/// Reasons a standing or a component is unknown, as fixed tokens.
const AUTHOR_UNRECORDED: &str = "author-unrecorded";
const READ_MODEL_UNRECORDED: &str = "read-model-unrecorded";
const READ_EFFORT_UNRECORDED: &str = "read-effort-unrecorded";
const READ_UNASSESSED: &str = "read-unassessed";
const READ_UNRANKED: &str = "read-unranked";
const FLOOR_UNKNOWN: &str = "floor-unknown";
const NO_FRONTIER: &str = "no-frontier";
const POLICY_UNREADABLE: &str = "policy-unreadable";
const HEAD_UNREADABLE: &str = "head-unreadable";
const SUBJECT_UNRECORDED: &str = "patchset-unrecorded";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ReadKind {
    Verdict,
    Audit,
}

/// One read a change received, as its event recorded it.
#[derive(Debug, Clone, Serialize)]
struct ReadView {
    event: String,
    kind: ReadKind,
    verdict: Verdict,
    /// The model string as recorded, effort included when it carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    route_version: Option<String>,
    /// Whether the read can clear the change: an approval of the shipped
    /// patchset, or an approving audit.
    counts: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Standing {
    /// An eligible read stands at or above the frontier.
    Fresh,
    /// Every eligible read stands below the frontier.
    Stale,
    /// No eligible read was recorded: the debt view says the same.
    Owed,
    /// The answer could not be had; `reason` says why.
    Unknown,
    /// Stale, but none of the change's lines remain to be read again.
    NoLiveLines,
}

impl Standing {
    fn label(self) -> &'static str {
        match self {
            Standing::Fresh => "fresh",
            Standing::Stale => "stale",
            Standing::Owed => "owed",
            Standing::Unknown => "unknown",
            Standing::NoLiveLines => "no-live-lines",
        }
    }

    /// The order a refresh queue lists them: stale work first.
    fn queue_rank(self) -> u8 {
        match self {
            Standing::Stale => 0,
            Standing::Unknown => 1,
            Standing::Owed => 2,
            Standing::NoLiveLines => 3,
            Standing::Fresh => 4,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct LiveFraction {
    #[serde(skip_serializing_if = "Option::is_none")]
    fraction: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    added: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ExposureState {
    /// The diff touches paths the project declared dangerous.
    Touched,
    Untouched,
    /// The project declares no dangerous paths, so none can be touched.
    Undeclared,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
struct Exposure {
    state: ExposureState,
    #[serde(skip_serializing_if = "Option::is_none")]
    paths: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
struct Components {
    /// Frontier tier minus the best eligible read's tier, as routing answered
    /// now. Absent when routing could not place the reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    frontier_gap: Option<i64>,
    live_fraction: LiveFraction,
    exposure: Exposure,
    /// When the newest read was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    newest_read_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    age_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct Judgement {
    standing: Standing,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    /// The models of the work's `Implemented-by` lines, else the recorder.
    authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    route_version: Option<String>,
    components: Components,
}

#[derive(Debug, Clone, Serialize)]
struct Row {
    change_id: String,
    slug: String,
    title: String,
    /// Where the change's declared review debt stands, when it declared one.
    #[serde(skip_serializing_if = "Option::is_none")]
    debt: Option<DebtStanding>,
    reads: Vec<ReadView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assurance: Option<Judgement>,
}

#[derive(Serialize)]
struct Report {
    schema: &'static str,
    /// The revision liveness was read at.
    #[serde(skip_serializing_if = "Option::is_none")]
    head: Option<String>,
    /// Whether routing standing and components were asked for.
    stale: bool,
    changes: Vec<Row>,
}

/// The patchset an integrated change shipped.
fn shipped(state: &ChangeState) -> Option<&Patchset> {
    state
        .debt_subject_patchset()
        .or_else(|| state.latest_patchset())
}

fn reads_of(state: &ChangeState, shipped: Option<&Patchset>) -> Vec<ReadView> {
    let mut reads = Vec::new();
    for verdict in &state.verdicts {
        reads.push(ReadView {
            event: verdict.event_id.clone(),
            kind: ReadKind::Verdict,
            verdict: verdict.verdict,
            effort: verdict
                .model
                .as_deref()
                .and_then(model_effort)
                .map(str::to_string),
            model: verdict.model.clone(),
            at: verdict.created_at,
            route_version: verdict.route_version.clone(),
            counts: verdict.verdict == Verdict::Approved
                && shipped.is_some_and(|patchset| patchset.id == verdict.patchset_id),
        });
    }
    for audit in &state.audit_verdicts {
        reads.push(ReadView {
            event: audit.event_id.clone(),
            kind: ReadKind::Audit,
            verdict: audit.verdict,
            effort: audit
                .model
                .as_deref()
                .and_then(model_effort)
                .map(str::to_string),
            model: audit.model.clone(),
            at: audit.created_at,
            route_version: audit.route_version.clone(),
            counts: audit.verdict == Verdict::Approved,
        });
    }
    reads.sort_by(|a, b| (a.at, &a.event).cmp(&(b.at, &b.event)));
    reads
}

/// A recorded model as routing names it: without the harness of a
/// `harness:model` coordinate.
fn routing_model(recorded: &str) -> Option<String> {
    let recorded = recorded.trim();
    let model = recorded.split_once(':').map_or(recorded, |(_, rest)| rest);
    (!model.is_empty() && !model.contains(char::is_whitespace)).then(|| model.to_string())
}

/// The coordinate routing places a read by, or why it has none.
fn read_coordinate(read: &ReadView) -> Result<String, &'static str> {
    let model = read
        .model
        .as_deref()
        .and_then(routing_model)
        .ok_or(READ_MODEL_UNRECORDED)?;
    match model_effort(&model) {
        Some(_) => Ok(model),
        None => Err(READ_EFFORT_UNRECORDED),
    }
}

/// The models whose work the change shipped: the production record's
/// `Implemented-by` values, else the shipped range's, else the recorder.
fn authors_of(cwd: &Path, state: &ChangeState, shipped: &Patchset) -> Vec<String> {
    let recorded = state
        .debt
        .as_ref()
        .and_then(|debt| debt.production.as_ref())
        .and_then(|production| production.trailer_implementers.clone());
    let values = recorded
        .or_else(|| crate::trailers::range_implementers(cwd, &shipped.base, &shipped.head).ok());
    let mut authors = Vec::<String>::new();
    for value in values.iter().flatten() {
        if let Some(author) = crate::trailers::coordinate_model_effort(value) {
            if !authors.iter().any(|seen| seen == author) {
                authors.push(author.to_string());
            }
        }
    }
    if authors.is_empty() {
        authors.extend(shipped.model.as_deref().and_then(routing_model));
    }
    authors
}

/// What one author's frontier says of the change's counting reads.
struct PerAuthor {
    standing: Standing,
    reason: Option<&'static str>,
    gap: Option<i64>,
    route_version: Option<String>,
}

fn unknown(reason: &'static str) -> PerAuthor {
    PerAuthor {
        standing: Standing::Unknown,
        reason: Some(reason),
        gap: None,
        route_version: None,
    }
}

/// Place the change's reads against one author's frontier. `unassessed` holds
/// a counting read nobody could ask routing about, which can never prove the
/// change stale or owed because it might stand at the frontier.
fn judge_author(
    answer: Result<Answer, routing::Reason>,
    unassessed: Option<&'static str>,
) -> PerAuthor {
    let answer = match answer {
        Ok(answer) => answer,
        Err(reason) => return unknown(reason),
    };
    let eligible = answer
        .reads
        .iter()
        .flatten()
        .filter(|read| read.eligible)
        .collect::<Vec<_>>();
    let refused = answer.reads.iter().any(Option::is_none);
    let mut judged = PerAuthor {
        standing: Standing::Unknown,
        reason: None,
        gap: None,
        route_version: answer.route_version.clone(),
    };
    if eligible.is_empty() {
        match unassessed.or(refused.then_some(READ_UNASSESSED)) {
            Some(reason) => judged.reason = Some(reason),
            None => judged.standing = Standing::Owed,
        }
        return judged;
    }
    if answer.status == "floor-unknown" {
        judged.reason = Some(FLOOR_UNKNOWN);
        return judged;
    }
    if answer.status != "frontier" {
        judged.reason = Some(NO_FRONTIER);
        return judged;
    }
    let best = eligible.iter().filter_map(|read| read.tier).max();
    judged.gap = answer
        .frontier_tier
        .zip(best)
        .map(|(frontier, best)| (frontier - best).max(0));
    if eligible
        .iter()
        .any(|read| matches!(read.placement, Placement::At | Placement::Above))
    {
        judged.standing = Standing::Fresh;
    } else if eligible
        .iter()
        .any(|read| read.placement == Placement::Unranked)
    {
        judged.reason = Some(READ_UNRANKED);
    } else if let Some(reason) = unassessed.or(refused.then_some(READ_UNASSESSED)) {
        judged.reason = Some(reason);
    } else {
        judged.standing = Standing::Stale;
    }
    judged
}

/// Stale for any author makes the change stale; an author with no eligible
/// read leaves it owed; an author nobody could place leaves it unknown.
fn combine(judged: Vec<PerAuthor>) -> PerAuthor {
    let pick = |standing: Standing| judged.iter().filter(move |one| one.standing == standing);
    let first = |standing: Standing| pick(standing).next();
    let route_version = judged.iter().find_map(|one| one.route_version.clone());
    let mut combined = if let Some(stale) = first(Standing::Stale) {
        PerAuthor {
            standing: Standing::Stale,
            reason: None,
            gap: pick(Standing::Stale).filter_map(|one| one.gap).max(),
            route_version: stale.route_version.clone(),
        }
    } else if first(Standing::Owed).is_some() {
        PerAuthor {
            standing: Standing::Owed,
            reason: None,
            gap: None,
            route_version: None,
        }
    } else if let Some(unplaced) = first(Standing::Unknown) {
        PerAuthor {
            standing: Standing::Unknown,
            reason: unplaced.reason,
            gap: None,
            route_version: None,
        }
    } else {
        PerAuthor {
            standing: Standing::Fresh,
            reason: None,
            gap: pick(Standing::Fresh).filter_map(|one| one.gap).max(),
            route_version: None,
        }
    };
    combined.route_version = combined.route_version.or(route_version);
    combined
}

struct View<'a> {
    cwd: &'a Path,
    cache: PathBuf,
    head: Option<String>,
    danger: Result<Danger, &'static str>,
    routing: Routing,
    now: DateTime<Utc>,
}

impl View<'_> {
    fn standing(
        &mut self,
        state: &ChangeState,
        shipped: Option<&Patchset>,
        reads: &[ReadView],
    ) -> (PerAuthor, Vec<String>) {
        let counting = reads.iter().filter(|read| read.counts).collect::<Vec<_>>();
        if counting.is_empty() {
            return (
                PerAuthor {
                    standing: Standing::Owed,
                    reason: None,
                    gap: None,
                    route_version: None,
                },
                Vec::new(),
            );
        }
        let Some(shipped) = shipped else {
            return (unknown(SUBJECT_UNRECORDED), Vec::new());
        };
        let authors = authors_of(self.cwd, state, shipped);
        if authors.is_empty() {
            return (unknown(AUTHOR_UNRECORDED), authors);
        }
        let mut coordinates = Vec::<String>::new();
        let mut unassessed = None;
        for read in &counting {
            match read_coordinate(read) {
                Ok(coordinate) => {
                    if !coordinates.contains(&coordinate) {
                        coordinates.push(coordinate);
                    }
                }
                Err(reason) => unassessed = unassessed.or(Some(reason)),
            }
        }
        if coordinates.is_empty() {
            return (
                unknown(unassessed.unwrap_or(READ_MODEL_UNRECORDED)),
                authors,
            );
        }
        coordinates.sort();
        let judged = authors
            .iter()
            .map(|author| judge_author(self.routing.frontier(author, &coordinates), unassessed))
            .collect::<Vec<_>>();
        (combine(judged), authors)
    }

    fn live_fraction(&self, change_id: &str, shipped: Option<&Patchset>) -> LiveFraction {
        let unknown = |reason| LiveFraction {
            fraction: None,
            added: None,
            live: None,
            reason: Some(reason),
        };
        let (Some(head), Some(patchset)) = (self.head.as_deref(), shipped) else {
            return unknown(if self.head.is_none() {
                HEAD_UNREADABLE
            } else {
                SUBJECT_UNRECORDED
            });
        };
        match live::survival(
            self.cwd,
            &self.cache,
            head,
            change_id,
            &patchset.base,
            &patchset.head,
        ) {
            Ok(survival @ Survival { added, live }) => LiveFraction {
                fraction: survival.fraction(),
                added: Some(added),
                live: Some(live),
                reason: None,
            },
            Err(reason) => unknown(reason),
        }
    }

    fn exposure(&self, shipped: Option<&Patchset>) -> Exposure {
        let unknown = |reason| Exposure {
            state: ExposureState::Unknown,
            paths: None,
            reason: Some(reason),
        };
        let danger = match &self.danger {
            Ok(danger) => danger,
            Err(reason) => return unknown(reason),
        };
        if !danger.is_declared() {
            return Exposure {
                state: ExposureState::Undeclared,
                paths: None,
                reason: None,
            };
        }
        let Some(patchset) = shipped else {
            return unknown(SUBJECT_UNRECORDED);
        };
        let Ok(changed) = crate::gitio::changed_paths(self.cwd, &patchset.base, &patchset.head)
        else {
            return unknown(live::RANGE_UNREADABLE);
        };
        let touched = danger.matching(changed.iter().map(String::as_str)).len();
        Exposure {
            state: if touched > 0 {
                ExposureState::Touched
            } else {
                ExposureState::Untouched
            },
            paths: Some(touched),
            reason: None,
        }
    }

    fn judgement(
        &mut self,
        state: &ChangeState,
        shipped: Option<&Patchset>,
        reads: &[ReadView],
    ) -> Judgement {
        let (placed, authors) = self.standing(state, shipped, reads);
        let live_fraction = self.live_fraction(&state.change_id, shipped);
        let newest = reads.iter().map(|read| read.at).max();
        // A change none of whose lines remain has nothing to refresh, whatever
        // routing seats now. Only a measured zero says so.
        let (standing, reason) = match placed.standing {
            Standing::Stale if live_fraction.live == Some(0) => (Standing::NoLiveLines, None),
            other => (other, placed.reason),
        };
        Judgement {
            standing,
            reason,
            authors,
            route_version: placed.route_version,
            components: Components {
                frontier_gap: placed.gap,
                live_fraction,
                exposure: self.exposure(shipped),
                newest_read_at: newest,
                age_seconds: newest.map(|at| (self.now - at).num_seconds().max(0)),
            },
        }
    }
}

fn integrated(state: &ChangeState) -> bool {
    state
        .closure
        .as_ref()
        .is_some_and(|closure| closure.outcome == Closure::Integrated)
}

/// The order of a refresh queue: stale work before unknown before owed; among
/// stale changes the largest frontier gap, then the most live lines, then the
/// most dangerous paths touched, then the oldest read.
fn queue_order(a: &Row, b: &Row) -> std::cmp::Ordering {
    let key = |row: &Row| {
        let judged = row.assurance.as_ref().expect("a queue row is judged");
        let fraction = judged.components.live_fraction.fraction;
        (
            judged.standing.queue_rank(),
            std::cmp::Reverse(judged.components.frontier_gap),
            std::cmp::Reverse(fraction.map(|value| (value * 1_000_000.0) as i64)),
            std::cmp::Reverse(judged.components.exposure.paths),
            std::cmp::Reverse(judged.components.age_seconds),
            row.change_id.clone(),
        )
    };
    key(a).cmp(&key(b))
}

/// Print the assurance of every integrated change in `states`.
pub fn print(
    cwd: &Path,
    ledger: &Path,
    selected: &[&ChangeState],
    stale: bool,
    json: bool,
) -> Result<()> {
    let head = crate::gitio::head(cwd).ok();
    let mut view = View {
        cwd,
        cache: ledger.join("cache").join("assurance"),
        danger: match &head {
            Some(head) => crate::policy::load_at(cwd, head)
                .map(|file| file.danger)
                .map_err(|_| POLICY_UNREADABLE),
            None => Err(HEAD_UNREADABLE),
        },
        head: head.clone(),
        routing: Routing::default(),
        now: Utc::now(),
    };
    let mut rows = Vec::new();
    for state in selected.iter().filter(|state| integrated(state)) {
        let shipped = shipped(state);
        let reads = reads_of(state, shipped);
        let assurance = stale.then(|| view.judgement(state, shipped, &reads));
        rows.push(Row {
            change_id: state.change_id.clone(),
            slug: state.slug.clone(),
            title: state.title.clone(),
            debt: state.debt_standing(),
            reads,
            assurance,
        });
    }
    if stale {
        rows.retain(|row| {
            row.assurance
                .as_ref()
                .is_some_and(|judged| judged.standing != Standing::Fresh)
        });
        rows.sort_by(queue_order);
    }
    if json {
        let report = Report {
            schema: SCHEMA,
            head,
            stale,
            changes: rows,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_text(&rows, stale);
    }
    Ok(())
}

fn print_text(rows: &[Row], stale: bool) {
    if rows.is_empty() {
        println!(
            "{}",
            if stale {
                "no integrated change is stale, owed or unknown"
            } else {
                "no integrated changes"
            }
        );
        return;
    }
    for row in rows {
        let debt = row.debt.map_or("none", DebtStanding::label);
        println!("{}  {}  (debt: {debt})", row.change_id, row.title);
        for read in &row.reads {
            println!(
                "  {} {} {}  {}  {}{}",
                match read.kind {
                    ReadKind::Verdict => "verdict",
                    ReadKind::Audit => "audit",
                },
                match read.verdict {
                    Verdict::Approved => "approved",
                    Verdict::ChangesRequested => "changes-requested",
                    Verdict::CommentOnly => "comment-only",
                },
                read.model.as_deref().unwrap_or("model not recorded"),
                read.at.format("%Y-%m-%dT%H:%M:%SZ"),
                read.route_version
                    .as_deref()
                    .map_or("unrouted".to_string(), |version| format!("route {version}")),
                if read.counts {
                    ""
                } else {
                    "  (does not clear)"
                },
            );
        }
        if let (true, Some(judged)) = (stale, &row.assurance) {
            print_judgement(judged);
        }
    }
}

fn print_judgement(judged: &Judgement) {
    let reason = judged
        .reason
        .map_or(String::new(), |reason| format!(" ({reason})"));
    println!("  standing: {}{reason}", judged.standing.label());
    let components = &judged.components;
    let gap = components
        .frontier_gap
        .map_or("unknown".to_string(), |gap| format!("{gap} tier(s)"));
    let live = match (
        &components.live_fraction.fraction,
        &components.live_fraction.reason,
    ) {
        (Some(fraction), _) => format!(
            "{:.0}% ({} of {} lines)",
            fraction * 100.0,
            components.live_fraction.live.unwrap_or(0),
            components.live_fraction.added.unwrap_or(0)
        ),
        (None, Some(reason)) => format!("unknown ({reason})"),
        (None, None) => "unknown".to_string(),
    };
    let exposure = match (components.exposure.state, components.exposure.reason) {
        (ExposureState::Touched, _) => {
            format!(
                "{} dangerous path(s)",
                components.exposure.paths.unwrap_or(0)
            )
        }
        (ExposureState::Untouched, _) => "none".to_string(),
        (ExposureState::Undeclared, _) => "no dangerous paths declared".to_string(),
        (ExposureState::Unknown, reason) => {
            format!("unknown ({})", reason.unwrap_or("unreadable"))
        }
    };
    let age = components
        .age_seconds
        .map_or("no reads".to_string(), |seconds| {
            format!("{}d", seconds / 86_400)
        });
    println!("  frontier gap: {gap}  live: {live}  exposure: {exposure}  age: {age}");
}
