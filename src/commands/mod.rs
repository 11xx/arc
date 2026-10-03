use crate::bundle::{Bundle, ValidatedBundle};
mod audit;
mod bundle_io;
pub(crate) mod candidate;
mod chain;
mod changelog;
mod claims;
mod config_cmd;
mod diff;
mod doctor;
mod external;
mod findings;
mod forge_cmd;
pub(crate) mod fork;
mod gatekeeping;
mod history;
mod hooks;
mod lifecycle;
pub(crate) mod messaging;
mod observe;
mod pass;
mod policy_cmd;
pub(crate) mod relations;
mod replica;
mod rescue;
pub(crate) mod review;
mod rewrite;
pub mod run;
pub(crate) mod sandbox;
pub(crate) mod scaffold;
pub(crate) mod selection;
mod squash;
pub(crate) use scaffold::{
    available as scaffolds_available, default_for_kind as scaffold_default_for_kind,
    resolve as scaffold_resolve, BUILT_IN as SCAFFOLD_BUILT_IN,
};
mod stats;
mod timeline;
mod workspace;

use crate::config;
use crate::gates;
use crate::gitio;
use crate::ids;
use crate::model::*;
use crate::render;
use crate::state::{self, ChangeState};
use crate::status::{self, StatusReport};
use crate::store::{Store, TransitionLock};
use anyhow::{bail, Context, Result};
pub use audit::{audit, declare_debt, AuditArgs};
pub use bundle_io::{export_bundle, import_bundle};
pub use chain::chain;
pub use changelog::changelog;
pub use claims::{claim, release_claim, stage, take};
use clap::ValueEnum;
pub use config_cmd::check_writable;
pub use diff::{diff, DiffArgs};
pub use doctor::run as run_doctor;
pub use external::{record_verdict as record_external_verdict, ExternalVerdictArgs};
pub use findings::{findings, FindingsFormat};
pub use forge_cmd::{forge_checks, forge_declare, forge_link, forge_pr_state};
pub(crate) use gatekeeping::dependency_order;
pub use gatekeeping::{
    check_selection, close, done, hold, integrate, rebase, release_hold, snapshot_with_verify,
    verify, CloseArgs, DebtDeclaration, IntegrateArgs, VerifyArgs,
};
pub use history::{record_rewrite, resolve_rewritten, withdraw_rewrite};
pub use hooks::{
    hook_run, install as hooks_install, query_commit, status as hooks_status,
    uninstall as hooks_uninstall,
};
pub use lifecycle::{
    begin, blocker_status_cmd, brief, is_blocked, iterating, list, metadata, query, read_metadata,
    show_selection, status_cmd,
};
pub(crate) use lifecycle::{print_projected, status_output};
pub use messaging::{catchup, inbox, message, messages};
pub use observe::{events, watch, EventsArgs, WatchArgs, WatchQuorum};
pub use pass::{abandon_pass, complete_pass, list_passes, open_pass};
pub use policy_cmd::{path as policy_path, show as policy_show, write as policy_write};
pub use replica::{
    confirm_return as replica_confirm_return, export as replica_export, id as replica_id,
    import as replica_import, init as replica_init, offer as replica_offer, pair as replica_pair,
    reclaim as replica_reclaim, status as replica_status,
};
pub use rescue::rescue;
pub use review::{comment, finding, keep, read_review, reply, resolve, review, ReviewArgs};
pub use rewrite::{
    instructions_git, sign as rewrite_sign, trailers as rewrite_trailers, SignArgs, TrailerArgs,
};
pub use run::{dispatch_run, end_run, list_runs, DispatchInput, EndingInput};
use serde::Serialize;
pub use squash::squash;
pub use stats::{stats, StatsSelection, StatsView};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
pub use timeline::log;
pub use workspace::{
    report_rules_help, restack, workspace, RankBasis, StorageSelection, WorkspaceScope,
    WorkspaceView,
};

/// The commands that record nothing, in every form and flag. An invocation of
/// one reads the ledger through a handle that never parses the invoking
/// checkout's append policy and refuses every append, and the guide prints its
/// list from here.
pub(crate) const READ_ONLY_COMMANDS: &[&str] = &[
    "status",
    "show",
    "check",
    "explain",
    "findings",
    "log",
    "list",
    "query",
    "blocker-status",
    "is-blocked",
    "messages",
    "inbox",
    "catchup",
];

pub struct Ctx {
    pub cwd: PathBuf,
    /// The invocation is one of `READ_ONLY_COMMANDS`.
    pub reads_only: bool,
    pub actor: String,
    /// Where `actor` came from. A fallback identity is announced the first
    /// time an event would carry it, because an operator cannot correct an
    /// append-only misattribution after the fact.
    pub actor_source: ActorSource,
    /// The checkout's Git identity when `actor` was derived from the harness
    /// session, recorded on each event as its operator.
    pub operator: Option<String>,
    /// Set once the fallback warning has been printed, so one command says it
    /// once however many events it appends.
    pub fallback_announced: std::cell::Cell<bool>,
    pub harness: Option<String>,
    pub session: Option<String>,
    /// Private session link stored in events, never in Git-published text.
    pub session_link: Option<String>,
    /// What the harness's own store said about `session` when detection
    /// resolved it. `None` when the session was declared or detection did not
    /// run. Model resolution at event creation is a separate observation.
    pub session_resolution: Option<SessionResolution>,
    /// Declared model slug with optional `#effort`. An absent declaration
    /// leaves the invocation to resolve its model from the acting session store.
    pub model: Option<String>,
    pub model_source: Option<ModelSource>,
    /// Attribution shared by every event and checkout view of this invocation.
    pub model_attribution: std::rc::Rc<std::cell::OnceCell<ModelAttribution>>,
    /// Subject a lead runs delegated ceremony for (`--on-behalf-of`). The
    /// effective author of any event is `on_behalf_of.unwrap_or(actor)`.
    pub on_behalf_of: Option<String>,
}

const POLL_MIN: Duration = Duration::from_millis(100);
const POLL_MAX: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum WatchUntil {
    Snapshot,
    Stalled,
    Reviewed,
    Approved,
    GatesGreen,
    Ready,
    Blocked,
    BriefRecorded,
    Integrated,
    Closed,
}

impl WatchUntil {
    fn label(self) -> &'static str {
        match self {
            WatchUntil::Snapshot => "snapshot",
            WatchUntil::Stalled => "stalled",
            WatchUntil::Reviewed => "reviewed",
            WatchUntil::Approved => "approved",
            WatchUntil::GatesGreen => "gates-green",
            WatchUntil::Ready => "ready",
            WatchUntil::Blocked => "blocked",
            WatchUntil::BriefRecorded => "brief-recorded",
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
    let state = store.state(&change_id)?;
    Ok((change_id, transition, state))
}

/// A pause the test suite injects into a command, so an act that lands
/// mid-command is exercised on purpose. `var` names an environment variable
/// holding a file path; the command waits for that file to exist. Unset,
/// which is every run that is not a test, this does nothing.
fn pause_until_released(var: &str) -> Result<()> {
    let Some(release) = std::env::var_os(var) else {
        return Ok(());
    };
    let release = Path::new(&release);
    let deadline = Instant::now() + Duration::from_secs(60);
    while !release.exists() {
        if Instant::now() >= deadline {
            bail!("{var}: {} never appeared", release.display());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn ensure_append_allowed(state: &ChangeState, payload: &Payload) -> Result<()> {
    let permission = append_permission(payload);
    let Some(closure) = &state.closure else {
        return match permission {
            AppendPermission::OpenOnly
            | AppendPermission::AnyPhaseFact
            | AppendPermission::OpenOrIntegratedFact => Ok(()),
            AppendPermission::IntegratedOnlyFact => {
                bail!("event may be recorded only after integration")
            }
            AppendPermission::LifecycleOwned | AppendPermission::OpaqueImported => {
                bail!("event lifecycle is owned internally")
            }
        };
    };

    let outcome = match closure.outcome {
        Closure::Integrated => "integrated",
        Closure::Abandoned => "abandoned",
        Closure::Superseded => "superseded",
    };
    match permission {
        AppendPermission::AnyPhaseFact => Ok(()),
        AppendPermission::OpenOrIntegratedFact if closure.outcome == Closure::Integrated => Ok(()),
        AppendPermission::IntegratedOnlyFact if closure.outcome == Closure::Integrated => Ok(()),
        AppendPermission::OpenOnly => {
            bail!(
                "change {} is {outcome}; event is open-only",
                state.change_id
            )
        }
        AppendPermission::OpenOrIntegratedFact => {
            let (subject, verb) = if matches!(payload, Payload::ChangelogRecorded { .. }) {
                ("changelog entries", "require")
            } else {
                ("event", "requires")
            };
            bail!(
                "change {} is {outcome}; {subject} {verb} an open or integrated change",
                state.change_id
            )
        }
        AppendPermission::IntegratedOnlyFact => bail!(
            "change {} is {outcome}; event requires an integrated change",
            state.change_id
        ),
        AppendPermission::LifecycleOwned | AppendPermission::OpaqueImported => {
            bail!("event lifecycle is owned internally")
        }
    }
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
    /// The ledger handle every path of this invocation reads and appends
    /// through. A read-only command's handle never parses the invoking
    /// checkout's append policy and refuses every append.
    pub(crate) fn store(&self) -> Result<Store> {
        if self.reads_only {
            Store::discover_for_reading(&self.cwd)
        } else {
            Store::discover(&self.cwd)
        }
    }

    pub(crate) fn resolve_model(&self) -> ModelAttribution {
        self.model_attribution
            .get_or_init(|| {
                crate::context::resolve_model(
                    self.model.as_deref(),
                    self.model_source,
                    self.harness.as_deref(),
                    self.session.as_deref(),
                )
            })
            .clone()
    }

    /// The same invocation pointed at another checkout.
    ///
    /// Identity travels with the command, not with the directory, so a run
    /// that has to happen in a different tree — a batch working from the
    /// primary worktree, a gate run belonging to the change's own — records
    /// the same actor, harness, session, and subject it would have recorded
    /// where it was typed.
    pub(crate) fn with_cwd(&self, cwd: PathBuf) -> Ctx {
        Ctx {
            cwd,
            reads_only: self.reads_only,
            actor: self.actor.clone(),
            actor_source: self.actor_source,
            operator: self.operator.clone(),
            fallback_announced: self.fallback_announced.clone(),
            harness: self.harness.clone(),
            session: self.session.clone(),
            session_link: self.session_link.clone(),
            session_resolution: self.session_resolution,
            model: self.model.clone(),
            model_source: self.model_source,
            model_attribution: self.model_attribution.clone(),
            on_behalf_of: self.on_behalf_of.clone(),
        }
    }

    /// The sandbox prefix that puts `path` out of this invocation's reach, or
    /// `None` when arc may run a command there and write beneath it.
    ///
    /// Every lookup that follows a path out of a record asks here, so one rule
    /// answers for all of them: gates, replays, spool promotion, and worktree
    /// cleanup either agree about what is in reach or the boundary has a hole.
    pub(crate) fn excluded_by_sandbox(&self, path: &Path) -> Result<Option<PathBuf>> {
        config::excluded_by_sandbox(&self.repository(), path)
    }

    /// Whether arc may run a command in `path` and write beneath it.
    pub(crate) fn admits_path(&self, path: &Path) -> bool {
        config::admits_path(&self.repository(), path)
    }

    /// The repository this invocation was pointed at, which a sandbox admits
    /// alongside its prefix: arc reads and writes the ledger of the repository
    /// it is run in wherever that repository sits. An invocation outside a
    /// repository has only the directory it was typed in.
    fn repository(&self) -> PathBuf {
        gitio::primary_worktree(&self.cwd).unwrap_or_else(|_| self.cwd.clone())
    }

    fn event(&self, store: &Store, change_id: &str, payload: Payload) -> Event {
        self.event_at(store, change_id, chrono::Utc::now(), payload)
    }

    /// Refuse now, if this repository requires a declared actor and nobody
    /// declared one.
    ///
    /// The append itself is guarded, but `begin`, `verify`, and `integrate` do
    /// irreversible work before they record anything — a branch, an arbitrary
    /// command, a merge — and any of those landing while the ledger refuses
    /// the event is a worse outcome than either answer on its own. The answer
    /// comes from the store the command is about to write to, so one
    /// invocation is judged by one reading of the policy.
    pub(crate) fn ensure_declared_actor(&self, store: &Store) -> Result<()> {
        store.refuse_read_only()?;
        if self.actor_source.declared() || self.on_behalf_of.is_some() {
            return Ok(());
        }
        if !store.require_declared_actor {
            return Ok(());
        }
        self.undeclared_actor_refusal(&store.require_declared_actor_sources)
    }

    /// Refuse now, if the change's target requires a declared actor and nobody
    /// declared one. The store answers for the checkout the command was typed
    /// in, which a change can edit; the target's policy is the one integration
    /// answers to.
    pub(crate) fn ensure_target_declares_actor(&self, state: &ChangeState) -> Result<()> {
        if self.actor_source.declared() || self.on_behalf_of.is_some() {
            return Ok(());
        }
        let declarations = crate::declarations::for_change(&self.cwd, state)?;
        // An unreadable target is refused by readiness, with the blocker that
        // names it, rather than here as an unexplained error.
        if declarations.target_unreadable {
            return Ok(());
        }
        let policy = declarations.policy;
        if !policy.policy.require_declared_actor {
            return Ok(());
        }
        self.undeclared_actor_refusal(
            &policy
                .sources
                .sources_for("policy.require_declared_actor=true"),
        )
    }

    fn undeclared_actor_refusal(&self, sources: &[String]) -> Result<()> {
        let source = if sources.is_empty() {
            "source unavailable".to_string()
        } else {
            sources.join(", ")
        };
        bail!(
            "policy requires a declared actor (declared by {source}): {:?} came from {}, which nobody claimed. Pass --actor or set ARC_ACTOR.",
            self.actor,
            self.actor_source.assumed_from().unwrap_or_default()
        )
    }

    /// Say what identity is about to be recorded when nobody declared one.
    /// The value is permanent once appended, so the moment to notice is now.
    fn announce_assumed_identity(&self) {
        if self.actor_source.declared()
            || self.on_behalf_of.is_some()
            || self.fallback_announced.replace(true)
        {
            return;
        }
        eprintln!(
            "warning: recording actor {:?} from {}; nobody declared one. \
             Pass --actor or set ARC_ACTOR.",
            self.actor,
            self.actor_source.assumed_from().unwrap_or_default()
        );
    }

    /// Name the identity a verdict is recorded under when that identity also
    /// wrote the work, or when arc assumed it rather than anyone declaring it.
    ///
    /// A repository that permits self-approval still gets an honest record.
    /// The verdict stands — it is a fact about what somebody concluded — and
    /// the line says which fact keeps it from being an independent one, so an
    /// approval attributed to the author is visible at the moment it is
    /// written rather than only to whoever later reads the ledger. Reports
    /// whether it said anything, so a caller can skip a weaker line it made
    /// redundant.
    pub(crate) fn warn_verdict_not_independent(
        &self,
        patchset: &crate::state::Patchset,
        verdict: Verdict,
    ) -> bool {
        if verdict != Verdict::Approved {
            return false;
        }
        let author = self.on_behalf_of.as_deref().unwrap_or(&self.actor);
        let assumed = self
            .on_behalf_of
            .is_none()
            .then(|| self.actor_source.assumed_from())
            .flatten();
        let matched = patchset.contributor_match(author).is_some();
        if !matched && assumed.is_none() {
            return false;
        }
        let mut line = format!("warning: recorded as {author:?}");
        if matched {
            let role = if patchset.contributors.is_empty() {
                "the author of"
            } else {
                "a contributor to"
            };
            line.push_str(&format!(", who is {role} {}", patchset.id));
        }
        if let Some(source) = assumed {
            line.push_str(&format!(" (identity assumed from {source})"));
        }
        line.push_str(
            "; this verdict shows that a review happened, not that it was independent of \
             whoever wrote the work, so it cannot discharge an independent-review debt.",
        );
        eprintln!("{line}");
        true
    }

    fn event_at(
        &self,
        store: &Store,
        change_id: &str,
        created_at: chrono::DateTime<chrono::Utc>,
        payload: Payload,
    ) -> Event {
        self.announce_assumed_identity();
        let attribution = self.resolve_model();
        Event {
            schema_version: SCHEMA_VERSION,
            event_id: ids::new_event_id(),
            repository_id: store.repository_id.clone(),
            change_id: change_id.to_string(),
            actor: self.actor.clone(),
            actor_source: Some(self.actor_source),
            operator: self.operator.clone(),
            on_behalf_of: self.on_behalf_of.clone(),
            model: attribution.model,
            model_provenance: attribution.provenance,
            harness: self.harness.clone(),
            session: self.session.clone(),
            session_link: self.session_link.clone(),
            session_resolution: self.session_resolution,
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
        let mut state = state::reduce_following(&events, &store.rewrites()?)?;
        crate::replica::localize_change(&store.repository_id, &events, &mut state);
        Ok((change_id, state))
    }

    pub(crate) fn load_all_states(&self, store: &Store) -> Result<BTreeMap<String, ChangeState>> {
        let mut states = BTreeMap::new();
        let rewrites = store.rewrites()?;
        for change_id in store.list_change_ids()? {
            let events = store.load_events(&change_id)?;
            let mut state = state::reduce_following(&events, &rewrites)?;
            crate::replica::localize_change(&store.repository_id, &events, &mut state);
            states.insert(change_id, state);
        }
        Ok(states)
    }

    /// The declarations this change is judged by, wherever this command runs.
    ///
    /// Refuses when the target branch cannot be resolved: nothing then says
    /// what the change owes, and a run or a merge must not proceed on a guess.
    pub(crate) fn declarations(
        &self,
        state: &ChangeState,
    ) -> Result<crate::declarations::Declarations> {
        let declarations = crate::declarations::for_change(&self.cwd, state)?;
        if declarations.target_unreadable {
            bail!(
                "target branch {:?} of {} cannot be resolved, so the gate and policy \
                 declarations the change is judged by could not be read",
                state.target_branch,
                state.change_id
            );
        }
        Ok(declarations)
    }

    pub(crate) fn report(&self, store: &Store, state: &ChangeState) -> Result<StatusReport> {
        self.report_with_states(state, &store.readable_states()?)
    }

    pub(crate) fn report_with_states(
        &self,
        state: &ChangeState,
        states: &BTreeMap<String, ChangeState>,
    ) -> Result<StatusReport> {
        let declarations = crate::declarations::for_change(&self.cwd, state)?;
        status::build(
            state,
            &self.cwd,
            &declarations,
            dependency_status(state, states),
            changes_blocked_by(&state.change_id, states),
            fork::fork_slug_for_branch(&self.cwd, &state.branch)?,
        )
    }

    /// Build a report for a state replayed to a past event: the latest
    /// patchset's head stands in for the live branch head, or the head the
    /// closure recorded once the replayed state is closed, so approval
    /// validity reflects what the actor saw at that point rather than the
    /// current worktree. Cross-change dependency state is still evaluated
    /// against the present ledger.
    pub(crate) fn report_as_of(&self, store: &Store, state: &ChangeState) -> Result<StatusReport> {
        let toplevel = gitio::toplevel(&self.cwd)?;
        let declarations = crate::declarations::for_change(&self.cwd, state)?;
        let states = store.readable_states()?;
        let mut report = status::build_as_of(
            state,
            &declarations.gates,
            &declarations.policy,
            declarations.target_unreadable,
            dependency_status(state, &states),
            changes_blocked_by(&state.change_id, &states),
            chrono::Utc::now(),
            Some(toplevel.as_path()),
            fork::fork_slug_for_branch(&self.cwd, &state.branch)?,
        )?;
        report.declaration_notes = declarations.notes;
        Ok(report)
    }
}

/// Replay a change's ledger up to and including `event_id`, answering
/// "what did an actor see at this point?". Rejects an event ID absent from
/// this change's ledger.
pub(crate) fn reduce_at(store: &Store, change_id: &str, event_id: &str) -> Result<ChangeState> {
    let events = store.load_events(change_id)?;
    let position = events
        .iter()
        .position(|event| event.event_id == event_id)
        .with_context(|| format!("unknown event {event_id:?} in {change_id}"))?;
    let prefix = &events[..=position];
    let mut state = state::reduce_following(prefix, &store.rewrites()?)?;
    crate::replica::localize_change(&store.repository_id, prefix, &mut state);
    Ok(state)
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

pub(crate) fn dependency_status(
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

pub(crate) fn changes_blocked_by(
    change_id: &str,
    states: &BTreeMap<String, ChangeState>,
) -> Vec<String> {
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
                && candidate.holds.is_empty()
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

/// The permission bits `write_atomically` gives the file it publishes.
pub enum FileMode {
    /// A file being created: the bits are requested and the umask applies.
    Create(u32),
    /// A file being replaced: exactly these bits, whatever the umask.
    Keep(u32),
}

/// Replace `path` with `contents` so a reader sees the old file or the new
/// one and never a partial write. The bytes go to a sibling temporary file
/// given `mode`, are synced, and are renamed over `path`; the directory is
/// synced after. The rename is the commit point: a failure before it leaves
/// `path` as it was, and a failure after it (opening or syncing the
/// directory) is reported with the new contents already in place but not
/// yet known durable. The temporary file is removed either way.
pub fn write_atomically(path: &Path, contents: &[u8], mode: FileMode) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let (create_mode, exact_mode) = match mode {
        FileMode::Create(bits) => (bits, None),
        FileMode::Keep(bits) => (0o600, Some(bits)),
    };

    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    let name = path
        .file_name()
        .with_context(|| format!("{} names no file", path.display()))?;
    let temp = parent.join(format!(
        ".{}-{}.tmp",
        name.to_string_lossy(),
        crate::ids::new_event_id()
    ));
    let publish = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(create_mode)
            .open(&temp)
            .with_context(|| format!("cannot create {}", temp.display()))?;
        if let Some(bits) = exact_mode {
            file.set_permissions(std::fs::Permissions::from_mode(bits))
                .with_context(|| format!("cannot set the mode of {}", temp.display()))?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
            .with_context(|| format!("cannot publish {}", path.display()))?;
        std::fs::File::open(parent)
            .with_context(|| format!("cannot open {}", parent.display()))?
            .sync_all()
            .with_context(|| format!("cannot sync {}", parent.display()))?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temp);
    publish
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

/// A duration in the units `parse_duration` reads: hours and minutes, with
/// seconds kept below an hour where they still matter to a reader.
pub(crate) fn format_duration(seconds: u64) -> String {
    if seconds == 0 {
        return "0s".into();
    }
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    let mut out = String::new();
    if hours > 0 {
        out.push_str(&format!("{hours}h"));
    }
    if minutes > 0 {
        out.push_str(&format!("{minutes}m"));
    }
    if secs > 0 && hours == 0 {
        out.push_str(&format!("{secs}s"));
    }
    out
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
    /// Only changes carrying an undischarged review obligation.
    pub debt: bool,
    /// Only changes whose gating approval is still owed corroboration.
    pub provisional: bool,
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
            schema: crate::state::CHANGE_STATE_SCHEMA,
            model_attributions: BTreeMap::new(),
            dirty_tree_waiver: None,
            dangerous: false,
            kept: Vec::new(),
            change_id: id.into(),
            slug: id.trim_end_matches("-id").into(),
            title: id.into(),
            profile: "local".into(),
            iterating: false,
            target_branch: "master".into(),
            branch: format!("arc/{id}"),
            base: "base".into(),
            worktree: None,
            opened_by: "tester".into(),
            opened_on_behalf_of: None,
            opened_harness: Some("test".into()),
            opened_model: None,
            opened_session: Some("session".into()),
            opened_session_link: None,
            journal_ref: None,
            journal_ref_digest: None,
            from_fork: None,
            blocked_by: blocked_by.iter().map(|id| (*id).into()).collect(),
            tags: Vec::new(),
            assigned_to: None,
            priority: 0,
            opened_at: Utc::now(),
            patchsets: Vec::new(),
            briefs: Vec::new(),
            changelog: None,
            messages: Vec::new(),
            comments: Vec::new(),
            findings: BTreeMap::new(),
            verdicts: Vec::new(),
            external_verdicts: Vec::new(),
            ready_to_send: None,
            audit_verdicts: Vec::new(),
            audit_findings: Default::default(),
            debt: None,
            blocked_on_stages: Vec::new(),
            verifications: Vec::new(),
            verification_runs: Vec::new(),
            claim: None,
            retired_claim_ids: BTreeSet::new(),
            holds: BTreeMap::new(),
            forge: crate::forge::ForgeState::default(),
            closure: closure.map(|outcome| crate::state::ClosureState {
                outcome,
                integration: None,
                source_patchset_id: None,
                source_head: None,
                target_branch: None,
                target_before: None,
                authorization: None,
                external_reference: None,
                integrated_commit: None,
                superseded_by: None,
                event_id: "event".into(),
                created_at: Utc::now(),
            }),
        }
    }

    #[test]
    fn find_unblocked_changes_returns_only_open_ready_work() {
        let mut held = change("d-id", &[], None);
        held.holds.insert(
            "01HOLD".into(),
            crate::state::HoldState {
                hold_event_id: "01HOLD".into(),
                reason: "waiting for user".into(),
                held_by: "lead".into(),
                created_at: Utc::now(),
            },
        );
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
