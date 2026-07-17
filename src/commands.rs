use crate::bundle::{Bundle, ValidatedBundle};
use crate::gates;
use crate::gitio;
use crate::ids;
use crate::model::*;
use crate::render;
use crate::state::{self, ChangeState};
use crate::status::{self, StatusReport};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
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

impl Ctx {
    fn store(&self) -> Result<Store> {
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

    fn load_state(&self, store: &Store, reference: &str) -> Result<(String, ChangeState)> {
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

    fn report(&self, store: &Store, state: &ChangeState) -> Result<StatusReport> {
        let toplevel = gitio::toplevel(&self.cwd)?;
        let gates = gates::load(&toplevel)?;
        let states = self.load_all_states(store)?;
        status::build(
            state,
            &self.cwd,
            &gates,
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

fn find_unblocked_changes(
    current_change_id: &str,
    states: &BTreeMap<String, ChangeState>,
) -> Vec<ArcAlternative> {
    states
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
        })
        .collect()
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

/// Replay raw ledger events as compact NDJSON, optionally continuing as new
/// event files arrive. Event IDs are ULIDs, so replay and each observed polling
/// batch can be sorted across changes; concurrent appends may cross batches.
pub fn events(
    ctx: &Ctx,
    follow: bool,
    change: Option<&str>,
    event_type: Option<&str>,
) -> Result<()> {
    let store = ctx.store()?;
    let change_id = change
        .map(|reference| store.resolve_change(reference))
        .transpose()?;
    let mut seen = BTreeSet::new();
    let mut poll_interval = POLL_MIN;

    loop {
        let raw_events = match &change_id {
            Some(id) => store.raw_events_unseen(id, &seen)?,
            None => store.raw_events_all_unseen(&seen)?,
        };
        let observed_events = !raw_events.is_empty();
        let mut out = std::io::stdout().lock();
        for (event_id, value) in raw_events {
            seen.insert(event_id);
            if !event_type.is_none_or(|wanted| {
                value.get("event_type").and_then(serde_json::Value::as_str) == Some(wanted)
            }) {
                continue;
            }
            serde_json::to_writer(&mut out, &value)?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
        if !follow {
            return Ok(());
        }
        poll_interval = if observed_events {
            POLL_MIN
        } else {
            (poll_interval * 2).min(POLL_MAX)
        };
        thread::sleep(poll_interval);
    }
}

pub fn watch(
    ctx: &Ctx,
    reference: &str,
    until: WatchUntil,
    timeout_secs: Option<u64>,
) -> Result<i32> {
    let deadline = timeout_secs.map(|timeout| Instant::now() + Duration::from_secs(timeout));
    let result = gitio::with_deadline(deadline, || {
        watch_until_reached(ctx, reference, until, deadline)
    });
    match result {
        Ok(true) => {
            println!("reached: {}", until.label());
            Ok(0)
        }
        Ok(false) => {
            println!("timeout: {}", until.label());
            Ok(2)
        }
        Err(_) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
            println!("timeout: {}", until.label());
            Ok(2)
        }
        Err(error) => Err(error),
    }
}

fn watch_until_reached(
    ctx: &Ctx,
    reference: &str,
    until: WatchUntil,
    deadline: Option<Instant>,
) -> Result<bool> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let mut poll_interval = POLL_MIN;
    loop {
        if watch_reached(ctx, &store, &change_id, until)? {
            return Ok(true);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(false);
        }
        // Poll the derived condition itself rather than gating checks on event
        // discovery. `ready` also depends on live Git state. Backoff keeps idle
        // watchers cheap while retaining sub-second response for fresh work.
        let sleep_for = deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .map_or(poll_interval, |remaining| poll_interval.min(remaining));
        thread::sleep(sleep_for);
        poll_interval = (poll_interval * 2).min(POLL_MAX);
    }
}

fn watch_reached(ctx: &Ctx, store: &Store, change_id: &str, until: WatchUntil) -> Result<bool> {
    let events = store.load_events(change_id)?;
    let state = state::reduce(&events)?;
    Ok(match until {
        WatchUntil::Snapshot => state.latest_patchset().is_some(),
        WatchUntil::Stalled => state
            .claim
            .as_ref()
            .is_some_and(|claim| state::claim_timing_at(claim, chrono::Utc::now()).stale),
        WatchUntil::Ready => ctx.report(store, &state)?.integrate_ready,
        WatchUntil::Integrated => state
            .closure
            .as_ref()
            .is_some_and(|closure| closure.outcome == Closure::Integrated),
        WatchUntil::Closed => state.is_closed(),
    })
}

pub fn claim(
    ctx: &Ctx,
    reference: &str,
    ttl: Option<String>,
    stage_budgets: Vec<String>,
) -> Result<i32> {
    let owner = command_identity(ctx)?;
    let ttl_seconds = ttl
        .as_deref()
        .map(parse_duration)
        .transpose()?
        .unwrap_or(2 * 60 * 60);
    let mut budgets = default_stage_budgets();
    for raw in stage_budgets {
        let (key, seconds) = parse_stage_budget(&raw)?;
        budgets.insert(key, seconds);
    }

    let store = ctx.store()?;
    let (change_id, state) = ctx.load_state(&store, reference)?;
    let now = chrono::Utc::now();
    if state.is_closed() {
        bail!("change {change_id} is closed");
    }
    if let Some(existing) = &state.claim {
        let timing = state::claim_timing_at(existing, now);
        if timing.active && existing.owner != owner {
            print_claim_conflict("claim is already held", existing, &timing);
            return Ok(8);
        }
    }

    let event = ctx.event_at(
        &store,
        &change_id,
        now,
        Payload::ClaimSet {
            ttl_seconds,
            stage_budgets: budgets,
        },
    );
    store.append_event(&event)?;
    println!("claimed: {change_id} for {ttl_seconds}s");
    println!("event: {}", event.event_id);
    Ok(0)
}

pub fn release_claim(ctx: &Ctx, reference: &str) -> Result<i32> {
    let _caller = command_identity(ctx)?;
    let store = ctx.store()?;
    let (change_id, state) = ctx.load_state(&store, reference)?;
    let now = chrono::Utc::now();
    let Some(existing) = &state.claim else {
        eprintln!("claim conflict: {change_id} has no live claim");
        return Ok(8);
    };
    let timing = state::claim_timing_at(existing, now);
    if !timing.active {
        print_claim_conflict("claim is expired", existing, &timing);
        return Ok(8);
    }
    let event = ctx.event_at(&store, &change_id, now, Payload::ClaimReleased);
    store.append_event(&event)?;
    println!("claim released: {change_id}");
    println!("event: {}", event.event_id);
    Ok(0)
}

pub fn stage(ctx: &Ctx, reference: &str, stage: StageArg, note: Option<String>) -> Result<i32> {
    let owner = command_identity(ctx)?;
    let stage = ClaimStage::from(stage);
    let note = note.and_then(|note| {
        let trimmed = note.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    });
    if stage == ClaimStage::BlockedOn && note.is_none() {
        bail!("blocked-on requires a nonempty --note");
    }

    let store = ctx.store()?;
    let (change_id, state) = ctx.load_state(&store, reference)?;
    let now = chrono::Utc::now();
    let Some(existing) = &state.claim else {
        eprintln!(
            "claim conflict: {change_id} has no claim for stage {}",
            stage.as_str()
        );
        return Ok(8);
    };
    let timing = state::claim_timing_at(existing, now);
    if !timing.active {
        print_claim_conflict("claim is expired", existing, &timing);
        return Ok(8);
    }
    if existing.owner != owner {
        print_claim_conflict("stage caller does not own claim", existing, &timing);
        return Ok(8);
    }

    let event = ctx.event_at(&store, &change_id, now, Payload::StageSet { stage, note });
    store.append_event(&event)?;
    println!("stage: {}", stage.as_str());
    println!("event: {}", event.event_id);
    Ok(0)
}

fn command_identity(ctx: &Ctx) -> Result<state::ClaimIdentity> {
    let harness = ctx
        .harness
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("claim/stage commands require nonempty ARC_HARNESS or --harness")?;
    let session = ctx
        .session
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("claim/stage commands require nonempty ARC_SESSION or --session")?;
    Ok(state::ClaimIdentity {
        actor: ctx.actor.clone(),
        harness: harness.to_string(),
        session: session.to_string(),
    })
}

fn print_claim_conflict(message: &str, claim: &state::ClaimState, timing: &state::ClaimTiming) {
    eprintln!(
        "claim conflict: {message}; owner={} harness={} session={} stage={}",
        claim.owner.actor, claim.owner.harness, claim.owner.session, timing.stage
    );
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

fn parse_stage_budget(raw: &str) -> Result<(StageBudget, u64)> {
    let (name, duration) = raw
        .split_once('=')
        .with_context(|| format!("invalid stage budget {raw:?}; expected <name>=<duration>"))?;
    let key = match name {
        "launch" => StageBudget::Launch,
        "started" => StageBudget::Started,
        "spec-read" => StageBudget::SpecRead,
        "implementing" => StageBudget::Implementing,
        "verifying" => StageBudget::Verifying,
        _ => bail!(
            "unknown stage budget {name:?}; expected launch, started, spec-read, implementing, or verifying"
        ),
    };
    Ok((key, parse_duration(duration)?))
}

fn default_stage_budgets() -> BTreeMap<StageBudget, u64> {
    [
        (StageBudget::Launch, 60),
        (StageBudget::Started, 5 * 60),
        (StageBudget::SpecRead, 2 * 60),
        (StageBudget::Implementing, 30 * 60),
        (StageBudget::Verifying, 15 * 60),
    ]
    .into_iter()
    .collect()
}

#[allow(clippy::too_many_arguments)]
pub fn begin(
    ctx: &Ctx,
    slug: &str,
    title: Option<String>,
    profile: &str,
    target: Option<String>,
    base: Option<String>,
    branch: Option<String>,
    worktree: Option<String>,
    no_worktree: bool,
    adopt: Option<String>,
    blocked_by: Vec<String>,
    tags: Vec<String>,
) -> Result<()> {
    ids::validate_slug(slug)?;
    let store = ctx.store()?;
    let blocked_by = blocked_by
        .iter()
        .map(|reference| store.resolve_change(reference))
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    let tags = normalize_tags(tags)?;

    let mut open_change_branches: Vec<String> = Vec::new();
    for existing in store.list_change_ids()? {
        let events = store.load_events(&existing)?;
        let st = state::reduce(&events)?;
        if st.is_closed() {
            continue;
        }
        if st.slug == slug {
            bail!(
                "open change {existing} already uses slug {slug:?}; continue it or close it first"
            );
        }
        open_change_branches.push(st.branch);
    }

    // Changes derive from the branch they intend to merge into. The
    // default is the primary worktree's branch (the main checkout,
    // normally master/main) — never whatever branch happens to be
    // checked out here, which may itself be work in progress. Stacking
    // on another open change requires an explicit --target.
    let explicit_target = target.is_some();
    let target_branch = match target {
        Some(t) => t,
        None => gitio::primary_worktree_branch(&ctx.cwd)?
            .or(gitio::current_branch(&ctx.cwd)?)
            .context("cannot determine a target branch (detached?); pass --target")?,
    };
    if !explicit_target && open_change_branches.contains(&target_branch) {
        bail!(
            "default target {target_branch:?} is another open change's branch; \
             pass --target explicitly to stack changes deliberately"
        );
    }
    let target_head = gitio::branch_head(&ctx.cwd, &target_branch)?;

    let change_id = ids::new_change_id(slug);
    let title = title.unwrap_or_else(|| slug.replace('-', " "));

    let (branch_name, base_rev, worktree_path) = if let Some(adopted) = adopt {
        if !gitio::branch_exists(&ctx.cwd, &adopted) {
            bail!("--adopt branch {adopted:?} does not exist");
        }
        let branch_head = gitio::branch_head(&ctx.cwd, &adopted)?;
        let base_rev = match base {
            Some(b) => gitio::rev_parse(&ctx.cwd, &b)?,
            None => gitio::merge_base(&ctx.cwd, &target_head, &branch_head)?,
        };
        let wt = gitio::worktree_for_branch(&ctx.cwd, &adopted)?.map(|p| p.display().to_string());
        (adopted, base_rev, wt)
    } else {
        let branch_name = branch.unwrap_or_else(|| format!("arc/{slug}"));
        if gitio::branch_exists(&ctx.cwd, &branch_name) {
            bail!("branch {branch_name:?} already exists; use --adopt {branch_name} to track it");
        }
        let base_rev = match base {
            Some(b) => gitio::rev_parse(&ctx.cwd, &b)?,
            None => target_head.clone(),
        };
        gitio::create_branch(&ctx.cwd, &branch_name, &base_rev)?;
        let wt = if no_worktree {
            None
        } else {
            let path = match worktree {
                Some(p) => PathBuf::from(p),
                None => default_worktree_path(&ctx.cwd, slug)?,
            };
            if path.exists() {
                bail!("worktree path {} already exists", path.display());
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
            gitio::add_worktree(&ctx.cwd, &path, &branch_name)?;
            Some(path.display().to_string())
        };
        (branch_name, base_rev, wt)
    };

    let ev = ctx.event(
        &store,
        &change_id,
        Payload::ChangeOpened {
            slug: slug.to_string(),
            title,
            profile: profile.to_string(),
            target_branch,
            branch: branch_name.clone(),
            base: base_rev,
            worktree: worktree_path.clone(),
            blocked_by,
            tags,
        },
    );
    store.append_event(&ev)?;

    println!("change: {change_id}");
    println!("branch: {branch_name}");
    if let Some(wt) = worktree_path {
        println!("worktree: {wt}");
    }
    Ok(())
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

fn default_worktree_path(cwd: &Path, slug: &str) -> Result<PathBuf> {
    let toplevel = gitio::toplevel(cwd)?;
    let repo_name = toplevel
        .file_name()
        .context("cannot determine repository name")?
        .to_string_lossy()
        .into_owned();
    let config = crate::config::load()?;
    Ok(config.worktrees_dir.join(format!("{repo_name}-{slug}")))
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

pub fn list(ctx: &Ctx, open_only: bool, json: bool, format: ListFormat) -> Result<()> {
    let store = ctx.store()?;
    let states = ctx.load_all_states(&store)?;
    let selected = states
        .values()
        .filter(|state| !open_only || !state.is_closed())
        .collect::<Vec<_>>();

    if json || matches!(format, ListFormat::Json) {
        let rows = selected
            .iter()
            .map(|state| list_row(state, &states))
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if selected.is_empty() {
        println!("no changes");
    } else {
        match format {
            ListFormat::Compact => {
                for state in selected {
                    println!("{}", state.change_id);
                }
            }
            ListFormat::Wide => {
                println!(
                    "{:<36} {:<12} {:<18} {:<24} Target",
                    "Change", "Status", "Verdict", "Blocker"
                );
                for state in selected {
                    println!(
                        "{:<36} {:<12} {:<18} {:<24} {}",
                        state.change_id,
                        change_status(state),
                        verdict_label(state),
                        blocker_label(state, &states),
                        state.target_branch
                    );
                }
            }
            ListFormat::Default | ListFormat::Json => {
                for state in selected {
                    println!(
                        "{}  [{}] {} ({})",
                        state.change_id,
                        change_status(state),
                        state.title,
                        state.branch,
                    );
                }
            }
        }
    }
    Ok(())
}

pub fn query(ctx: &Ctx, args: QueryArgs) -> Result<()> {
    if let Some(status) = &args.status {
        if !matches!(
            status.as_str(),
            "open" | "closed" | "integrated" | "abandoned" | "superseded"
        ) {
            bail!(
                "unknown status {status:?}; expected open, closed, integrated, abandoned, or superseded"
            );
        }
    }
    let store = ctx.store()?;
    let states = ctx.load_all_states(&store)?;
    let tags = normalize_tags(args.tags)?;
    let selected = states
        .values()
        .filter(|state| {
            args.status
                .as_deref()
                .is_none_or(|wanted| status_matches(state, wanted))
                && args
                    .target
                    .as_deref()
                    .is_none_or(|target| state.target_branch == target)
                && tags.iter().all(|tag| state.tags.contains(tag))
                && args.verdict.is_none_or(|verdict| {
                    state.latest_verdict().is_some_and(|v| v.verdict == verdict)
                })
                && args
                    .actor
                    .as_deref()
                    .is_none_or(|actor| state.opened_by == actor)
                && args
                    .harness
                    .as_deref()
                    .is_none_or(|harness| state.opened_harness.as_deref() == Some(harness))
        })
        .collect::<Vec<_>>();

    if args.json {
        let rows = selected
            .iter()
            .map(|state| list_row(state, &states))
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        for state in selected {
            println!("{}", state.change_id);
        }
    }
    Ok(())
}

fn list_row(state: &ChangeState, states: &BTreeMap<String, ChangeState>) -> serde_json::Value {
    serde_json::json!({
        "change_id": state.change_id,
        "slug": state.slug,
        "title": state.title,
        "profile": state.profile,
        "branch": state.branch,
        "target_branch": state.target_branch,
        "state": if state.is_closed() { "closed" } else { "open" },
        "status": change_status(state),
        "verdict": verdict_label(state),
        "blocked_by": state.blocked_by,
        "blocker": blocker_label(state, states),
        "tags": state.tags,
    })
}

fn status_matches(state: &ChangeState, wanted: &str) -> bool {
    match wanted {
        "closed" => state.is_closed(),
        other => change_status(state) == other,
    }
}

fn change_status(state: &ChangeState) -> &'static str {
    match state.closure.as_ref().map(|closure| closure.outcome) {
        Some(Closure::Integrated) => "integrated",
        Some(Closure::Abandoned) => "abandoned",
        Some(Closure::Superseded) => "superseded",
        None => "open",
    }
}

fn verdict_label(state: &ChangeState) -> &'static str {
    match state.latest_verdict().map(|verdict| verdict.verdict) {
        Some(Verdict::Approved) => "approved",
        Some(Verdict::ChangesRequested) => "changes-requested",
        Some(Verdict::CommentOnly) => "comment-only",
        None => "none",
    }
}

fn blocker_label(state: &ChangeState, states: &BTreeMap<String, ChangeState>) -> String {
    let dependencies = dependency_status(state, states);
    if let Some(blocker) = dependencies
        .blockers_ready
        .iter()
        .find(|blocker| !blocker.integrated)
    {
        return format!("blocked-by:{}", blocker.slug);
    }
    if !state.open_blocking_findings().is_empty() {
        return format!("{} findings", state.open_blocking_findings().len());
    }
    if state.hold.is_some() {
        return "hold".into();
    }
    "—".into()
}

pub fn show_selection(
    ctx: &Ctx,
    reference: Option<&str>,
    tags: Vec<String>,
    json: bool,
) -> Result<()> {
    match (reference, tags.is_empty()) {
        (Some(reference), true) => show(ctx, reference, json),
        (None, false) => show_tagged(ctx, normalize_tags(tags)?, json),
        (Some(_), false) => bail!("provide a change or --tag, not both"),
        (None, true) => bail!("provide a change or at least one --tag"),
    }
}

fn show(ctx: &Ctx, reference: &str, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let (_, st) = ctx.load_state(&store, reference)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&st)?);
    } else {
        let states = ctx.load_all_states(&store)?;
        let report = ctx.report(&store, &st)?;
        let alternatives = if report.blocker_status.blocked {
            find_unblocked_changes(&st.change_id, &states)
        } else {
            Vec::new()
        };
        print!("{}", render::markdown(&st, &report, &alternatives));
    }
    Ok(())
}

fn show_tagged(ctx: &Ctx, tags: Vec<String>, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let states = ctx.load_all_states(&store)?;
    let selected = states
        .values()
        .filter(|state| tags.iter().all(|tag| state.tags.contains(tag)))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("no changes match tags {}", tags.join(", "));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&selected)?);
    } else {
        for state in selected {
            let report = ctx.report(&store, state)?;
            let alternatives = if report.blocker_status.blocked {
                find_unblocked_changes(&state.change_id, &states)
            } else {
                Vec::new()
            };
            print!("{}", render::markdown(state, &report, &alternatives));
        }
    }
    Ok(())
}

pub fn metadata(
    ctx: &Ctx,
    reference: &str,
    blocked_by: Vec<String>,
    remove_blocked_by: Vec<String>,
    tags: Vec<String>,
    remove_tags: Vec<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, state) = ctx.load_state(&store, reference)?;
    if state.is_closed() {
        bail!("change {change_id} is closed");
    }
    let states = ctx.load_all_states(&store)?;
    let add_blocked_by = blocked_by
        .iter()
        .map(|dependency| store.resolve_change(dependency))
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    let remove_blocked_by = remove_blocked_by
        .iter()
        .map(|dependency| resolve_blocker_removal(&store, &state, dependency))
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    for dependency in &add_blocked_by {
        if dependency == &change_id || dependency_reaches(dependency, &change_id, &states) {
            bail!("adding blocker {dependency} would create a dependency cycle");
        }
    }
    let add_tags = normalize_tags(tags)?;
    let remove_tags = normalize_tags(remove_tags)?;
    if add_blocked_by.is_empty()
        && remove_blocked_by.is_empty()
        && add_tags.is_empty()
        && remove_tags.is_empty()
    {
        bail!("provide at least one metadata change");
    }
    let event = ctx.event(
        &store,
        &change_id,
        Payload::MetadataUpdated {
            add_blocked_by,
            remove_blocked_by,
            add_tags,
            remove_tags,
        },
    );
    store.append_event(&event)?;
    println!("event: {}", event.event_id);
    Ok(())
}

fn resolve_blocker_removal(store: &Store, state: &ChangeState, reference: &str) -> Result<String> {
    if state.blocked_by.iter().any(|blocker| blocker == reference) {
        return Ok(reference.to_string());
    }
    let matches = state
        .blocked_by
        .iter()
        .filter(|blocker| blocker.starts_with(reference))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [blocker] => Ok((*blocker).clone()),
        [] => store.resolve_change(reference),
        _ => bail!(
            "ambiguous blocker {reference:?}: matches {}",
            matches
                .iter()
                .map(|blocker| blocker.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn dependency_reaches(start: &str, target: &str, states: &BTreeMap<String, ChangeState>) -> bool {
    let mut pending = vec![start];
    let mut visited = BTreeSet::new();
    while let Some(change_id) = pending.pop() {
        if change_id == target {
            return true;
        }
        if !visited.insert(change_id) {
            continue;
        }
        if let Some(state) = states.get(change_id) {
            pending.extend(state.blocked_by.iter().map(String::as_str));
        }
    }
    false
}

pub fn status_cmd(ctx: &Ctx, reference: &str) -> Result<()> {
    let store = ctx.store()?;
    let (_, st) = ctx.load_state(&store, reference)?;
    let states = ctx.load_all_states(&store)?;
    let report = ctx.report(&store, &st)?;
    let suggested_alternatives = if report.blocker_status.blocked {
        find_unblocked_changes(&st.change_id, &states)
    } else {
        Vec::new()
    };
    let output = StatusOutput {
        report,
        suggested_alternatives,
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

pub fn blocker_status_cmd(ctx: &Ctx, reference: &str) -> Result<()> {
    let store = ctx.store()?;
    let (_, state) = ctx.load_state(&store, reference)?;
    let states = ctx.load_all_states(&store)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&dependency_status(&state, &states))?
    );
    Ok(())
}

pub fn is_blocked(ctx: &Ctx, reference: &str) -> Result<i32> {
    let store = ctx.store()?;
    let (_, state) = ctx.load_state(&store, reference)?;
    let states = ctx.load_all_states(&store)?;
    let blocker_status = dependency_status(&state, &states);
    if blocker_status.blocked {
        for blocker in blocker_status
            .blockers_ready
            .iter()
            .filter(|blocker| !blocker.integrated)
        {
            println!("blocked by {} ({})", blocker.change_id, blocker.status);
        }
        Ok(1)
    } else {
        println!("ready: all prerequisite changes are integrated");
        Ok(0)
    }
}

pub fn export_bundle(ctx: &Ctx, reference: &str, output: &str) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let bundle = Bundle::export(&store, &change_id)?;
    let bytes = bundle.to_bytes()?;
    if output == "-" {
        std::io::stdout().write_all(&bytes)?;
        eprintln!("events: {}", bundle.event_count);
        eprintln!("sha256: {}", bundle.events_sha256);
        eprintln!("output: -");
    } else {
        std::fs::write(output, bytes)
            .with_context(|| format!("cannot write export bundle {output}"))?;
        println!("events: {}", bundle.event_count);
        println!("sha256: {}", bundle.events_sha256);
        println!("output: {output}");
    }
    Ok(())
}

pub fn import_bundle(ctx: &Ctx, input: &str, dry_run: bool) -> Result<i32> {
    let bytes = if input == "-" {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        bytes
    } else {
        std::fs::read(input).with_context(|| format!("cannot read import bundle {input}"))?
    };
    // Parsing validates every path-bearing ID, checksum, envelope, and
    // patchset field before the local store is inspected or created.
    let validated = Bundle::parse(&bytes)?;
    let root = Store::resolve_root(&ctx.cwd)?;
    let local_repository_id = Store::repository_id_at(&root)?;

    let mut new_events = Vec::new();
    let mut skipped_events = Vec::new();
    let mut conflicts = Vec::new();
    for event in &validated.events {
        match Store::raw_event_at(&root, &validated.bundle.change_id, &event.event_id)? {
            None => new_events.push(event.event_id.clone()),
            Some(existing) => match serde_json::from_slice::<serde_json::Value>(&existing) {
                Ok(value) if value == event.value => skipped_events.push(event.event_id.clone()),
                _ => conflicts.push(event.event_id.clone()),
            },
        }
    }

    let mut missing_objects = Vec::new();
    let mut pins = Vec::new();
    for patchset in &validated.patchsets {
        if !gitio::commit_exists(&ctx.cwd, &patchset.base)? {
            missing_objects.push((patchset.event_id.clone(), "base", patchset.base.clone()));
        }
        if gitio::commit_exists(&ctx.cwd, &patchset.head)? {
            pins.push((
                gitio::retention_ref(&validated.bundle.change_id, &patchset.patchset_id),
                patchset.head.clone(),
            ));
        } else {
            missing_objects.push((patchset.event_id.clone(), "head", patchset.head.clone()));
        }
    }

    print_import_report(
        &validated,
        local_repository_id.as_deref(),
        &new_events,
        &skipped_events,
        &conflicts,
        &missing_objects,
        &pins,
        dry_run,
    );
    if !conflicts.is_empty() {
        println!("aborted: no events or refs written");
        return Ok(1);
    }
    if dry_run {
        return Ok(0);
    }

    let store = Store::discover(&ctx.cwd)?;
    if local_repository_id.is_none() && store.repository_id != validated.bundle.repository_id {
        println!(
            "repository: bundle {} differs from local {} (expected for cross-machine import)",
            validated.bundle.repository_id, store.repository_id
        );
    }
    for event in &validated.events {
        if new_events.contains(&event.event_id) {
            store.append_raw_event(&validated.bundle.change_id, &event.event_id, &event.bytes)?;
        }
    }
    for (name, head) in pins {
        gitio::update_ref(&ctx.cwd, &name, &head)?;
    }
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn print_import_report(
    validated: &ValidatedBundle,
    local_repository_id: Option<&str>,
    new_events: &[String],
    skipped_events: &[String],
    conflicts: &[String],
    missing_objects: &[(String, &str, String)],
    pins: &[(String, String)],
    dry_run: bool,
) {
    if let Some(local) = local_repository_id {
        if local != validated.bundle.repository_id {
            println!(
                "repository: bundle {} differs from local {local} (expected for cross-machine import)",
                validated.bundle.repository_id
            );
        }
    }
    for event_id in new_events {
        println!("new: {event_id}");
    }
    for event_id in skipped_events {
        println!("skipped: {event_id}");
    }
    for event_id in conflicts {
        println!("conflict: {event_id}");
    }
    for (event_id, kind, oid) in missing_objects {
        println!("warning: event {event_id} is missing {kind} commit {oid}");
    }
    for (event_id, event_type) in &validated.unknown_event_types {
        println!("unknown event type: {event_id} {event_type} (preserved verbatim)");
    }
    for (name, head) in pins {
        if dry_run {
            println!("would pin: {name} -> {head}");
        } else {
            println!("pin: {name} -> {head}");
        }
    }
    println!(
        "summary: new={} skipped={} conflicts={} missing_objects={}",
        new_events.len(),
        skipped_events.len(),
        conflicts.len(),
        missing_objects.len()
    );
    if dry_run {
        println!("dry-run: no events or refs written");
    }
}

pub fn check_selection(ctx: &Ctx, reference: Option<&str>, tags: Vec<String>) -> Result<i32> {
    match (reference, tags.is_empty()) {
        (Some(reference), true) => check(ctx, reference),
        (None, false) => check_tagged(ctx, normalize_tags(tags)?),
        (Some(_), false) => bail!("provide a change or --tag, not both"),
        (None, true) => bail!("provide a change or at least one --tag"),
    }
}

fn check(ctx: &Ctx, reference: &str) -> Result<i32> {
    let store = ctx.store()?;
    let (_, st) = ctx.load_state(&store, reference)?;
    let report = ctx.report(&store, &st)?;
    if report.integrate_ready {
        println!("ready: all integration gates pass");
    } else {
        print!("{}", render::blocker_explanation(&st, &report));
    }
    Ok(status::check_exit_code(&report))
}

fn check_tagged(ctx: &Ctx, tags: Vec<String>) -> Result<i32> {
    let store = ctx.store()?;
    let states = ctx.load_all_states(&store)?;
    let selected = states
        .values()
        .filter(|state| tags.iter().all(|tag| state.tags.contains(tag)))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("no changes match tags {}", tags.join(", "));
    }
    let mut aggregate = 0;
    for state in selected {
        if state.is_closed() {
            println!("{}: {}", state.change_id, change_status(state));
            continue;
        }
        let report = ctx.report(&store, state)?;
        let code = status::check_exit_code(&report);
        println!(
            "{}: {}",
            state.change_id,
            if code == 0 { "ready" } else { "blocked" }
        );
        if code != 0 {
            print!("{}", render::blocker_explanation(state, &report));
            if aggregate == 0 {
                aggregate = code;
            }
        }
    }
    Ok(aggregate)
}

pub fn snapshot(ctx: &Ctx, reference: &str, base: Option<String>) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let head = gitio::branch_head(&ctx.cwd, &st.branch)?;
    let base_rev = match base {
        Some(b) => gitio::rev_parse(&ctx.cwd, &b)?,
        None => st.base.clone(),
    };
    if let Some(p) = st.latest_patchset() {
        if p.head == head && p.base == base_rev {
            println!("patchset: {} (unchanged)", p.id);
            return Ok(());
        }
    }
    let target_head = gitio::branch_head(&ctx.cwd, &st.target_branch).ok();
    let merge_base = target_head
        .as_deref()
        .and_then(|t| gitio::merge_base(&ctx.cwd, t, &head).ok());
    let identity = gitio::commit_identity(&ctx.cwd, &head)?;
    let patchset_id = format!("ps-{:02}", st.patchsets.len() + 1);
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::PatchsetAdded {
            patchset_id: patchset_id.clone(),
            base: base_rev,
            head: head.clone(),
            merge_base,
            author_name: Some(identity.author_name),
            author_email: Some(identity.author_email),
            committer_name: Some(identity.committer_name),
            committer_email: Some(identity.committer_email),
        },
    );
    store.append_event(&ev)?;
    // Pin this head with its own ref: reviewed heads must stay reachable
    // individually, even if the branch is rewound or deleted later.
    gitio::update_ref(
        &ctx.cwd,
        &gitio::retention_ref(&change_id, &patchset_id),
        &head,
    )?;
    println!("patchset: {patchset_id}");
    println!("head: {head}");
    println!("event: {}", ev.event_id);
    Ok(())
}

pub struct AnchorArgs {
    pub path: Option<String>,
    pub side: Side,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub context: Option<String>,
}

fn build_anchor(
    ctx: &Ctx,
    st: &ChangeState,
    patchset_id: Option<&str>,
    args: &AnchorArgs,
) -> Result<Option<Anchor>> {
    let Some(path) = &args.path else {
        if args.line_start.is_some() {
            bail!("--line requires --path");
        }
        return Ok(None);
    };
    let patchset = match patchset_id {
        Some(id) => st.patchsets.iter().find(|p| p.id == id),
        None => st.latest_patchset(),
    };
    let blob = patchset.and_then(|p| {
        let rev = match args.side {
            Side::Base => &p.base,
            Side::Head => &p.head,
        };
        gitio::blob_oid(&ctx.cwd, rev, path)
    });
    Ok(Some(Anchor {
        path: path.clone(),
        side: args.side,
        blob,
        line_start: args.line_start,
        line_end: args.line_end.or(args.line_start),
        context: args.context.clone(),
    }))
}

fn resolve_patchset_id(st: &ChangeState, patchset: Option<String>) -> Result<Option<String>> {
    match patchset {
        Some(id) => {
            if !st.patchsets.iter().any(|p| p.id == id) {
                bail!("unknown patchset {id:?}");
            }
            Ok(Some(id))
        }
        None => Ok(st.latest_patchset().map(|p| p.id.clone())),
    }
}

pub fn comment(
    ctx: &Ctx,
    reference: &str,
    body: String,
    patchset: Option<String>,
    anchor_args: &AnchorArgs,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let patchset_id = resolve_patchset_id(&st, patchset)?;
    let anchor = build_anchor(ctx, &st, patchset_id.as_deref(), anchor_args)?;
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::CommentAdded {
            body,
            patchset_id,
            anchor,
        },
    );
    store.append_event(&ev)?;
    println!("event: {}", ev.event_id);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn finding(
    ctx: &Ctx,
    reference: &str,
    summary: String,
    body: Option<String>,
    blocking: bool,
    severity: Severity,
    patchset: Option<String>,
    anchor_args: &AnchorArgs,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let patchset_id = resolve_patchset_id(&st, patchset)?;
    let anchor = build_anchor(ctx, &st, patchset_id.as_deref(), anchor_args)?;
    let finding_id = ids::new_finding_id();
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::FindingAdded {
            finding_id: finding_id.clone(),
            blocking,
            severity,
            summary,
            body,
            patchset_id,
            anchor,
        },
    );
    store.append_event(&ev)?;
    println!("finding: {finding_id}");
    println!("event: {}", ev.event_id);
    Ok(())
}

pub fn reply(ctx: &Ctx, reference: &str, parent_event_id: String, body: String) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let known = st.comments.iter().any(|c| c.event_id == parent_event_id)
        || st
            .findings
            .values()
            .any(|f| f.origin_event == parent_event_id);
    if !known {
        bail!("no comment or finding event {parent_event_id:?} in this change");
    }
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::ReplyAdded {
            parent_event_id,
            body,
        },
    );
    store.append_event(&ev)?;
    println!("event: {}", ev.event_id);
    Ok(())
}

pub fn resolve(
    ctx: &Ctx,
    reference: &str,
    finding: String,
    disposition: DispositionStatus,
    commit: Option<String>,
    evidence: Option<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let finding_id = st.resolve_finding_id(&finding)?;
    let commit = match commit {
        Some(c) => Some(gitio::rev_parse(&ctx.cwd, &c)?),
        None => None,
    };
    let supersedes: Vec<String> = st.findings[&finding_id]
        .tips()
        .iter()
        .map(|t| t.event_id.clone())
        .collect();
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::DispositionRecorded {
            finding_id: finding_id.clone(),
            status: disposition,
            commit,
            evidence,
            supersedes,
        },
    );
    store.append_event(&ev)?;
    println!("finding: {finding_id} → {disposition:?}");
    println!("event: {}", ev.event_id);
    Ok(())
}

pub fn review(
    ctx: &Ctx,
    reference: &str,
    verdict: Verdict,
    patchset: Option<String>,
    findings_json: Option<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let patchset_id = resolve_patchset_id(&st, patchset)?
        .context("no patchset to review; run `arc snapshot` first")?;

    let inline: Vec<InlineFinding> = match findings_json {
        None => Vec::new(),
        Some(src) => {
            let text = if src == "-" {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf
            } else {
                std::fs::read_to_string(&src)
                    .with_context(|| format!("cannot read findings file {src}"))?
            };
            let inputs: Vec<FindingInput> =
                serde_json::from_str(&text).context("malformed findings JSON")?;
            inputs
                .into_iter()
                .map(|f| {
                    let anchor = f.anchor.map(|a| {
                        let anchor_args = AnchorArgs {
                            path: Some(a.path),
                            side: a.side,
                            line_start: a.line_start,
                            line_end: a.line_end,
                            context: a.context,
                        };
                        build_anchor(ctx, &st, Some(&patchset_id), &anchor_args)
                            .ok()
                            .flatten()
                    });
                    InlineFinding {
                        finding_id: ids::new_finding_id(),
                        blocking: f.blocking,
                        severity: f.severity,
                        summary: f.summary,
                        body: f.body,
                        anchor: anchor.flatten(),
                    }
                })
                .collect()
        }
    };

    if verdict == Verdict::Approved && inline.iter().any(|f| f.blocking) {
        bail!("cannot approve while recording blocking findings in the same review");
    }

    let finding_ids: Vec<String> = inline.iter().map(|f| f.finding_id.clone()).collect();
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::VerdictRecorded {
            patchset_id: patchset_id.clone(),
            verdict,
            findings: inline,
        },
    );
    store.append_event(&ev)?;
    println!("verdict: {verdict:?} on {patchset_id}");
    for id in finding_ids {
        println!("finding: {id}");
    }
    println!("event: {}", ev.event_id);
    Ok(())
}

pub fn verify(
    ctx: &Ctx,
    reference: &str,
    gate: Option<String>,
    command: Option<String>,
) -> Result<i32> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let toplevel = gitio::toplevel(&ctx.cwd)?;
    let cmd = match (&gate, command) {
        (Some(name), None) => {
            let gates = gates::load(&toplevel)?;
            gates
                .gates
                .get(name)
                .with_context(|| format!("gate {name:?} not declared in .arc/gates.toml"))?
                .command
                .clone()
        }
        (None, Some(c)) => c,
        (Some(_), Some(_)) => bail!("--gate and --command are mutually exclusive"),
        (None, None) => bail!("provide --gate <name> or --command <cmd>"),
    };
    let revision = gitio::head(&ctx.cwd)?;
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());

    eprintln!("running: {cmd}");
    let started = std::time::Instant::now();
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .current_dir(&ctx.cwd)
        .status()
        .context("failed to run gate command")?;
    let duration_ms = started.elapsed().as_millis() as u64;
    let exit_code = out.code().unwrap_or(-1);
    let result = if out.success() {
        VerifyResult::Pass
    } else {
        VerifyResult::Fail
    };

    let ev = ctx.event(
        &store,
        &change_id,
        Payload::VerificationRecorded {
            gate,
            command: cmd,
            revision: revision.clone(),
            result,
            exit_code,
            duration_ms,
            hostname,
        },
    );
    store.append_event(&ev)?;
    let _ = st;
    println!("verification: {result:?} at {revision}");
    println!("event: {}", ev.event_id);
    Ok(if out.success() { 0 } else { 1 })
}

pub fn hold(ctx: &Ctx, reference: &str, reason: String) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let ev = ctx.event(&store, &change_id, Payload::HoldSet { reason });
    store.append_event(&ev)?;
    println!("hold set on {change_id}");
    Ok(())
}

pub fn release_hold(ctx: &Ctx, reference: &str, reason: Option<String>) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    if st.hold.is_none() {
        bail!("no active hold on {change_id}");
    }
    let ev = ctx.event(&store, &change_id, Payload::HoldReleased { reason });
    store.append_event(&ev)?;
    println!("hold released on {change_id}");
    Ok(())
}

pub fn integrate(
    ctx: &Ctx,
    reference: &str,
    into: Option<String>,
    message: Option<String>,
    cleanup: bool,
) -> Result<i32> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let report = ctx.report(&store, &st)?;
    if let Some(claim) = &st.claim {
        let timing = state::claim_timing_at(claim, chrono::Utc::now());
        let caller = state::ClaimIdentity {
            actor: ctx.actor.clone(),
            harness: ctx.harness.clone().unwrap_or_default(),
            session: ctx.session.clone().unwrap_or_default(),
        };
        if timing.active && claim.owner != caller {
            eprintln!(
                "warning: active foreign claim by {} via {}/{} at stage {}{}; integration remains lead-owned",
                claim.owner.actor,
                claim.owner.harness,
                claim.owner.session,
                timing.stage,
                if timing.stale { " (stale)" } else { "" }
            );
        }
    }
    if !report.integrate_ready {
        eprint!("{}", render::blocker_explanation(&st, &report));
        return Ok(status::check_exit_code(&report));
    }

    let target = into.unwrap_or_else(|| st.target_branch.clone());
    // The approved head, merged by exact SHA so a branch moved after
    // approval can never smuggle unreviewed commits into the merge.
    let approved_head = st
        .latest_patchset()
        .context("no patchset recorded")?
        .head
        .clone();

    let wt = gitio::worktree_for_branch(&ctx.cwd, &target)?
        .with_context(|| format!("no worktree has {target:?} checked out; check it out first"))?;
    if !gitio::is_clean(&wt)? {
        bail!("target worktree {} is not clean", wt.display());
    }
    let old_target = gitio::branch_head(&ctx.cwd, &target)?;
    let msg = message.unwrap_or_else(|| format!("merge({}): {}", st.slug, st.title));

    if let Err(e) = gitio::git(
        &wt,
        &["merge", "--no-ff", "--no-edit", "-m", &msg, &approved_head],
    ) {
        let _ = gitio::git(&wt, &["merge", "--abort"]);
        bail!("merge failed (aborted): {e}");
    }

    let merged = gitio::head(&wt)?;
    let parents = gitio::commit_parents(&wt, &merged)?;
    if parents != vec![old_target.clone(), approved_head.clone()] {
        bail!(
            "merge commit {merged} has unexpected parents {parents:?}; \
             expected [{old_target}, {approved_head}] — target moved during \
             integration, inspect before trusting this merge"
        );
    }

    let ev = ctx.event(
        &store,
        &change_id,
        Payload::ChangeClosed {
            outcome: Closure::Integrated,
            integrated_commit: Some(merged.clone()),
            superseded_by: None,
        },
    );
    store.append_event(&ev)?;
    release_retention_refs(ctx, &change_id, Some(&merged))?;

    println!("integrated: {merged}");
    println!("event: {}", ev.event_id);

    if cleanup {
        // Run cleanup git commands from the target worktree: ctx.cwd may be
        // inside the change worktree that is about to be removed.
        if let Some(wt_path) = &st.worktree {
            let p = PathBuf::from(wt_path);
            if p.exists() && p != wt {
                gitio::git(&wt, &["worktree", "remove", wt_path])?;
                println!("removed worktree {wt_path}");
            }
        }
        // -d refuses unless merged: exactly the safety we want.
        gitio::git(&wt, &["branch", "-d", &st.branch])?;
        println!("deleted branch {}", st.branch);
    }
    Ok(0)
}

pub fn close(
    ctx: &Ctx,
    reference: &str,
    integrated: Option<String>,
    abandoned: bool,
    superseded_by: Option<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    if st.is_closed() {
        bail!("change {change_id} is already closed");
    }
    let (payload, integrated_rev) = match (integrated, abandoned, superseded_by) {
        (Some(rev), false, None) => {
            let rev = gitio::rev_parse(&ctx.cwd, &rev)?;
            (
                Payload::ChangeClosed {
                    outcome: Closure::Integrated,
                    integrated_commit: Some(rev.clone()),
                    superseded_by: None,
                },
                Some(rev),
            )
        }
        (None, true, None) => (
            Payload::ChangeClosed {
                outcome: Closure::Abandoned,
                integrated_commit: None,
                superseded_by: None,
            },
            None,
        ),
        (None, false, Some(other)) => {
            let other_id = store.resolve_change(&other)?;
            (
                Payload::ChangeClosed {
                    outcome: Closure::Superseded,
                    integrated_commit: None,
                    superseded_by: Some(other_id),
                },
                None,
            )
        }
        _ => bail!("provide exactly one of --integrated <rev>, --abandoned, --superseded <change>"),
    };
    let ev = ctx.event(&store, &change_id, payload);
    store.append_event(&ev)?;
    release_retention_refs(ctx, &change_id, integrated_rev.as_deref())?;
    println!("closed: {change_id}");
    println!("event: {}", ev.event_id);
    Ok(())
}

/// Drop a change's retention refs only for heads proven reachable from
/// the integration commit. Everything else stays pinned: abandoned or
/// externally rewritten (squash/rebase) work must never become
/// GC-collectable through arc. Unpinning by hand remains possible with
/// `git update-ref -d refs/arc/keep/<change>/<patchset>`.
fn release_retention_refs(ctx: &Ctx, change_id: &str, integrated: Option<&str>) -> Result<()> {
    let refs = gitio::list_refs(&ctx.cwd, &gitio::retention_prefix(change_id))?;
    for (name, oid) in refs {
        let reachable = match integrated {
            Some(rev) => gitio::is_ancestor(&ctx.cwd, &oid, rev)?,
            None => false,
        };
        if reachable {
            let _ = gitio::delete_ref(&ctx.cwd, &name);
        } else {
            println!("kept {name}: head {oid} is not reachable from the integrated commit");
        }
    }
    Ok(())
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
            opened_at: Utc::now(),
            patchsets: Vec::new(),
            comments: Vec::new(),
            findings: BTreeMap::new(),
            verdicts: Vec::new(),
            verifications: Vec::new(),
            claim: None,
            hold: None,
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
