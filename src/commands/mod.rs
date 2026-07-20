use crate::bundle::{Bundle, ValidatedBundle};
mod bundle_io;
mod claims;
mod config_cmd;
mod diff;
mod doctor;
mod findings;
mod forge_cmd;
mod gatekeeping;
mod lifecycle;
mod messaging;
mod observe;
mod review;

use crate::gates;
use crate::gitio;
use crate::ids;
use crate::model::*;
use crate::render;
use crate::state::{self, ChangeState};
use crate::status::{self, StatusReport};
use crate::store::{Store, TransitionLock};
use anyhow::{bail, Context, Result};
pub use bundle_io::{export_bundle, import_bundle};
pub use claims::{claim, release_claim, stage, take};
use clap::ValueEnum;
pub use config_cmd::check_writable;
pub use diff::diff;
pub use doctor::run as run_doctor;
pub use findings::{findings, FindingsFormat};
pub use forge_cmd::{forge_checks, forge_declare, forge_link, forge_pr_state};
pub(crate) use gatekeeping::dependency_order;
pub use gatekeeping::{
    check_selection, close, done, hold, integrate, release_hold, snapshot_with_verify, verify,
    VerifyArgs,
};
pub use lifecycle::{
    begin, blocker_status_cmd, brief, is_blocked, list, metadata, query, show_selection, status_cmd,
};
pub(crate) use lifecycle::{print_projected, status_output};
pub use messaging::{inbox, message, messages};
pub use observe::{events, watch};
pub use review::{comment, finding, reply, resolve, review};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

pub struct Ctx {
    pub cwd: PathBuf,
    pub actor: String,
    pub harness: Option<String>,
    pub session: Option<String>,
}

const POLL_MIN: Duration = Duration::from_millis(100);
const POLL_MAX: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum WatchUntil {
    Snapshot,
    Stalled,
    Ready,
    Integrated,
    Closed,
}

impl WatchUntil {
    fn label(self) -> &'static str {
        match self {
            WatchUntil::Snapshot => "snapshot",
            WatchUntil::Stalled => "stalled",
            WatchUntil::Ready => "ready",
            WatchUntil::Integrated => "integrated",
            WatchUntil::Closed => "closed",
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum StageArg {
    Started,
    SpecRead,
    Implementing,
    Verifying,
    BlockedOn,
}

impl From<StageArg> for ClaimStage {
    fn from(stage: StageArg) -> Self {
        match stage {
            StageArg::Started => ClaimStage::Started,
            StageArg::SpecRead => ClaimStage::SpecRead,
            StageArg::Implementing => ClaimStage::Implementing,
            StageArg::Verifying => ClaimStage::Verifying,
            StageArg::BlockedOn => ClaimStage::BlockedOn,
        }
    }
}

fn locked_state(store: &Store, reference: &str) -> Result<(String, TransitionLock, ChangeState)> {
    let change_id = store.resolve_change(reference)?;
    let transition = store.lock_transition(&change_id)?;
    let state = state::reduce(&store.load_events(&change_id)?)?;
    Ok((change_id, transition, state))
}

fn event_id_after(previous: &str) -> Result<String> {
    let previous = previous
        .parse::<ulid::Ulid>()
        .with_context(|| format!("event ID {previous:?} is not a ULID"))?;
    let current = ulid::Ulid::new();
    if current > previous {
        Ok(current.to_string())
    } else {
        previous
            .increment()
            .context("event ID sequence overflowed")
            .map(|id| id.to_string())
    }
}

impl Ctx {
    pub(crate) fn store(&self) -> Result<Store> {
        Store::discover(&self.cwd)
    }

    fn event(&self, store: &Store, change_id: &str, payload: Payload) -> Event {
        self.event_at(store, change_id, chrono::Utc::now(), payload)
    }

    fn event_at(
        &self,
        store: &Store,
        change_id: &str,
        created_at: chrono::DateTime<chrono::Utc>,
        payload: Payload,
    ) -> Event {
        Event {
            schema_version: SCHEMA_VERSION,
            event_id: ids::new_event_id(),
            repository_id: store.repository_id.clone(),
            change_id: change_id.to_string(),
            actor: self.actor.clone(),
            harness: self.harness.clone(),
            session: self.session.clone(),
            created_at,
            payload,
        }
    }

    pub(crate) fn load_state(
        &self,
        store: &Store,
        reference: &str,
    ) -> Result<(String, ChangeState)> {
        let change_id = store.resolve_change(reference)?;
        let events = store.load_events(&change_id)?;
        let state = state::reduce(&events)?;
        Ok((change_id, state))
    }

    fn load_all_states(&self, store: &Store) -> Result<BTreeMap<String, ChangeState>> {
        let mut states = BTreeMap::new();
        for change_id in store.list_change_ids()? {
            let events = store.load_events(&change_id)?;
            states.insert(change_id, state::reduce(&events)?);
        }
        Ok(states)
    }

    pub(crate) fn report(&self, store: &Store, state: &ChangeState) -> Result<StatusReport> {
        let toplevel = gitio::toplevel(&self.cwd)?;
        let gates = gates::load(&toplevel)?;
        let policy = crate::policy::load(&toplevel)?;
        let states = self.load_all_states(store)?;
        status::build(
            state,
            &self.cwd,
            &gates,
            &policy,
            dependency_status(state, &states),
            changes_blocked_by(&state.change_id, &states),
        )
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArcAlternative {
    pub change_id: String,
    pub slug: String,
    pub status: String,
    pub reason: String,
    pub priority: i32,
}

#[derive(Debug, Serialize)]
pub struct StatusOutput {
    #[serde(flatten)]
    pub report: StatusReport,
    pub suggested_alternatives: Vec<ArcAlternative>,
}

fn dependency_status(
    state: &ChangeState,
    states: &BTreeMap<String, ChangeState>,
) -> status::BlockerStatus {
    let blockers_ready = state
        .blocked_by
        .iter()
        .map(|change_id| dependency_change_status(change_id, states))
        .collect::<Vec<_>>();
    status::BlockerStatus {
        schema: status::BLOCKER_STATUS_SCHEMA,
        blocked: blockers_ready.iter().any(|blocker| !blocker.integrated),
        blockers_ready,
    }
}

const WEDGED_DEPENDENCY_RECOVERY: &str =
    "prerequisite closed without integration: clear or retarget with arc metadata";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupersessionResolution {
    Integrated,
    Wedged,
}

fn dependency_change_status(
    change_id: &str,
    states: &BTreeMap<String, ChangeState>,
) -> status::DependencyChangeStatus {
    let Some(blocker) = states.get(change_id) else {
        return status::DependencyChangeStatus {
            change_id: change_id.into(),
            slug: change_id.into(),
            status: "missing".into(),
            integrated: false,
            recovery: None,
        };
    };

    match blocker.closure.as_ref().map(|closure| closure.outcome) {
        Some(Closure::Integrated) => status::DependencyChangeStatus {
            change_id: change_id.into(),
            slug: blocker.slug.clone(),
            status: "integrated".into(),
            integrated: true,
            recovery: None,
        },
        Some(Closure::Superseded)
            if resolve_supersession(change_id, states) == SupersessionResolution::Integrated =>
        {
            status::DependencyChangeStatus {
                change_id: change_id.into(),
                slug: blocker.slug.clone(),
                status: "superseded-integrated".into(),
                integrated: true,
                recovery: None,
            }
        }
        Some(Closure::Abandoned) | Some(Closure::Superseded) => status::DependencyChangeStatus {
            change_id: change_id.into(),
            slug: blocker.slug.clone(),
            status: "wedged".into(),
            integrated: false,
            recovery: Some(WEDGED_DEPENDENCY_RECOVERY.into()),
        },
        None => status::DependencyChangeStatus {
            change_id: change_id.into(),
            slug: blocker.slug.clone(),
            status: "open".into(),
            integrated: false,
            recovery: None,
        },
    }
}

fn resolve_supersession(
    change_id: &str,
    states: &BTreeMap<String, ChangeState>,
) -> SupersessionResolution {
    let mut current = change_id;
    let mut visited = BTreeSet::new();

    loop {
        if !visited.insert(current) {
            return SupersessionResolution::Wedged;
        }
        let Some(state) = states.get(current) else {
            return SupersessionResolution::Wedged;
        };
        let Some(closure) = &state.closure else {
            return SupersessionResolution::Wedged;
        };
        match closure.outcome {
            Closure::Integrated => return SupersessionResolution::Integrated,
            Closure::Abandoned => return SupersessionResolution::Wedged,
            Closure::Superseded => match closure.superseded_by.as_deref() {
                Some(successor) => current = successor,
                None => return SupersessionResolution::Wedged,
            },
        }
    }
}

fn changes_blocked_by(change_id: &str, states: &BTreeMap<String, ChangeState>) -> Vec<String> {
    states
        .values()
        .filter(|candidate| candidate.blocked_by.iter().any(|id| id == change_id))
        .map(|candidate| candidate.change_id.clone())
        .collect()
}

pub(crate) fn find_unblocked_changes(
    current_change_id: &str,
    states: &BTreeMap<String, ChangeState>,
) -> Vec<ArcAlternative> {
    let mut candidates = states
        .values()
        .filter(|candidate| {
            candidate.change_id != current_change_id
                && !candidate.is_closed()
                && candidate.hold.is_none()
                && !dependency_status(candidate, states).blocked
                && candidate.claim.as_ref().is_none_or(|claim| {
                    let timing = state::claim_timing_at(claim, chrono::Utc::now());
                    !timing.active || timing.stale
                })
        })
        .map(|candidate| ArcAlternative {
            change_id: candidate.change_id.clone(),
            slug: candidate.slug.clone(),
            status: "open".into(),
            reason: if candidate.blocked_by.is_empty() {
                "no blockers, independent work".into()
            } else {
                "all blockers integrated, ready to work".into()
            },
            priority: candidate.priority,
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        b.priority.cmp(&a.priority).then_with(|| {
            states[&a.change_id]
                .opened_at
                .cmp(&states[&b.change_id].opened_at)
        })
    });
    candidates
}

pub fn read_body(body: Option<String>, body_file: Option<String>) -> Result<String> {
    let text = match (body, body_file) {
        (Some(b), None) => b,
        (None, Some(f)) if f == "-" => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
        (None, Some(f)) => {
            std::fs::read_to_string(&f).with_context(|| format!("cannot read body file {f}"))?
        }
        (None, None) => bail!("provide --body or --body-file (use '-' for stdin)"),
        (Some(_), Some(_)) => bail!("--body and --body-file are mutually exclusive"),
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        bail!("body is empty");
    }
    Ok(trimmed.to_string())
}

pub fn read_body_file_verbatim(path: &str) -> Result<String> {
    if path == "-" {
        let mut body = String::new();
        std::io::stdin().read_to_string(&mut body)?;
        Ok(body)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("cannot read body file {path}"))
    }
}

pub fn parse_duration(raw: &str) -> Result<u64> {
    let Some((suffix_index, suffix)) = raw.char_indices().last() else {
        bail!("duration is empty; expected a positive integer followed by s, m, or h");
    };
    let number = &raw[..suffix_index];
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("invalid duration {raw:?}; expected a positive integer followed by s, m, or h");
    }
    let value: u64 = number
        .parse()
        .with_context(|| format!("invalid duration {raw:?}"))?;
    if value == 0 {
        bail!("duration must be positive");
    }
    let multiplier = match suffix {
        's' => 1,
        'm' => 60,
        'h' => 60 * 60,
        _ => bail!("invalid duration {raw:?}; expected an s, m, or h suffix"),
    };
    value
        .checked_mul(multiplier)
        .filter(|seconds| *seconds <= i64::MAX as u64)
        .context("duration is too large")
}

#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum ListFormat {
    #[default]
    Default,
    Wide,
    Compact,
    Json,
}

pub struct QueryArgs {
    pub status: Option<String>,
    pub target: Option<String>,
    pub tags: Vec<String>,
    pub verdict: Option<Verdict>,
    pub actor: Option<String>,
    pub harness: Option<String>,
    pub json: bool,
}

// A message joined with the change it belongs to, for the `messages` query.
#[derive(Debug, Serialize)]
struct MessageView<'a> {
    change_id: &'a str,
    event_id: &'a str,
    event_type: &'static str,
    message_type: MessageType,
    severity: MessageSeverity,
    summary: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a serde_json::Value>,
    actor: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    harness: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<&'a str>,
    created_at: chrono::DateTime<chrono::Utc>,
}

struct ImportEventPlan {
    new_events: Vec<String>,
    skipped_events: Vec<String>,
    conflicts: Vec<String>,
}

pub struct AnchorArgs {
    pub path: Option<String>,
    pub side: Side,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub context: Option<String>,
}

pub struct ForgeLinkArgs {
    pub pr_number: u64,
    pub url: String,
    pub base_repo: String,
    pub base_ref: String,
    pub head_repo: String,
    pub head_ref: String,
    pub head_sha: String,
}

fn normalize_tags(tags: Vec<String>) -> Result<Vec<String>> {
    let mut normalized = BTreeSet::new();
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() || tag.chars().any(char::is_whitespace) {
            bail!("tag must be non-empty and contain no whitespace: {tag:?}");
        }
        normalized.insert(tag.to_string());
    }
    Ok(normalized.into_iter().collect())
}

fn change_status(state: &ChangeState) -> &'static str {
    match state.closure.as_ref().map(|closure| closure.outcome) {
        Some(Closure::Integrated) => "integrated",
        Some(Closure::Abandoned) => "abandoned",
        Some(Closure::Superseded) => "superseded",
        None => "open",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn change(id: &str, blocked_by: &[&str], closure: Option<Closure>) -> ChangeState {
        ChangeState {
            change_id: id.into(),
            slug: id.trim_end_matches("-id").into(),
            title: id.into(),
            profile: "local".into(),
            target_branch: "master".into(),
            branch: format!("arc/{id}"),
            base: "base".into(),
            worktree: None,
            opened_by: "tester".into(),
            opened_harness: Some("test".into()),
            blocked_by: blocked_by.iter().map(|id| (*id).into()).collect(),
            tags: Vec::new(),
            assigned_to: None,
            priority: 0,
            opened_at: Utc::now(),
            patchsets: Vec::new(),
            briefs: Vec::new(),
            messages: Vec::new(),
            comments: Vec::new(),
            findings: BTreeMap::new(),
            verdicts: Vec::new(),
            verifications: Vec::new(),
            claim: None,
            retired_claim_ids: BTreeSet::new(),
            hold: None,
            forge: crate::forge::ForgeState::default(),
            closure: closure.map(|outcome| crate::state::ClosureState {
                outcome,
                integrated_commit: None,
                superseded_by: None,
                event_id: "event".into(),
            }),
        }
    }

    #[test]
    fn find_unblocked_changes_returns_only_open_ready_work() {
        let mut held = change("d-id", &[], None);
        held.hold = Some("waiting for user".into());
        let states = BTreeMap::from([
            (
                "a-id".into(),
                change("a-id", &[], Some(Closure::Integrated)),
            ),
            ("b-id".into(), change("b-id", &["a-id"], None)),
            ("c-id".into(), change("c-id", &["b-id"], None)),
            ("d-id".into(), held),
            ("e-id".into(), change("e-id", &[], Some(Closure::Abandoned))),
            ("f-id".into(), change("f-id", &[], None)),
        ]);

        let alternatives = find_unblocked_changes("c-id", &states);
        assert_eq!(
            alternatives
                .iter()
                .map(|alternative| alternative.change_id.as_str())
                .collect::<Vec<_>>(),
            vec!["b-id", "f-id"]
        );
        assert_eq!(
            alternatives[0].reason,
            "all blockers integrated, ready to work"
        );
        assert_eq!(alternatives[1].reason, "no blockers, independent work");
    }
}
