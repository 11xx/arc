//! Explicit identity and integration authority for file-paired Arc stores.
//!
//! Replica events describe a logical project and its members. The store's
//! repository ID remains the identity of the event-recording store, while
//! checkout paths and journal locations remain local facts.

use crate::commands::Ctx;
use crate::ids;
use crate::model::{Event, Payload};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const EVENT_SCHEMA: &str = "arc-replica-event/1";
const BUNDLE_SCHEMA: &str = "arc-replica-bundle/1";
const IMPORT_SCHEMA: &str = "arc-replica-import/1";
const STATE_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const STATE_LOCK_RETRY: Duration = Duration::from_millis(10);
pub const INTEGRATION_AUTHORITY_EXIT_CODE: i32 = 17;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ReplicaIdentity {
    pub name: String,
    pub repository_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplicaEvent {
    pub schema: String,
    pub event_id: String,
    pub project_id: String,
    pub repository_id: String,
    pub actor: ReplicaIdentity,
    pub recorded_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub created_at: DateTime<Utc>,
    pub payload: ReplicaPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event_type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ReplicaPayload {
    Initialized,
    Paired {
        peer: ReplicaIdentity,
    },
    AuthorityOffered {
        offer_id: String,
        to: ReplicaIdentity,
        parent_event_id: String,
    },
    AuthorityAcquired {
        offer_id: String,
    },
    AuthorityReclaimed {
        offer_id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplicaBundle {
    pub schema: String,
    pub project_id: String,
    pub source_replica_id: String,
    pub events: Vec<ReplicaEvent>,
    pub events_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportBatch {
    schema: String,
    source_replica_id: String,
    bundle_sha256: String,
    imported_at: DateTime<Utc>,
    events: Vec<ReplicaEvent>,
}

#[derive(Debug, Serialize)]
pub struct ReplicaId {
    pub schema: &'static str,
    pub repository_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplicaStatus {
    pub schema: &'static str,
    pub repository_id: String,
    pub project_id: Option<String>,
    pub local: Option<ReplicaIdentity>,
    pub peers: Vec<ReplicaIdentity>,
    pub authority: Option<AuthorityStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorityStatus {
    pub holder: Option<ReplicaIdentity>,
    pub offer_in_flight: Option<AuthorityOffer>,
    pub last_reclaim: Option<AuthorityReclaim>,
    /// Offers both acquired by their recipient and reclaimed by their offerer.
    /// The reclaim decides the holder, so every replica converges on one; the
    /// acquirer believed it held authority until it imported the reclaim, and
    /// may have integrated in that interval.
    pub contested: Vec<AuthorityContest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorityContest {
    pub offer_id: String,
    pub acquired_by: ReplicaIdentity,
    pub reclaimed_by: ReplicaIdentity,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorityOffer {
    pub offer_id: String,
    pub from: ReplicaIdentity,
    pub to: ReplicaIdentity,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorityReclaim {
    pub offer_id: String,
    pub from: ReplicaIdentity,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ClaimContest {
    pub local_replica: String,
    pub local_actor: String,
    pub incoming_replica: String,
    pub incoming_actor: String,
}

impl ClaimContest {
    pub fn render(&self, change_id: &str) -> String {
        format!(
            "claim contest on change {change_id}: replica {} holds a live local claim by {}; \
             incoming replica {} holds a live claim by {}",
            self.local_replica, self.local_actor, self.incoming_replica, self.incoming_actor
        )
    }
}

#[derive(Debug, Clone)]
struct ReplicaSnapshot {
    project_id: String,
    local: ReplicaIdentity,
    members: BTreeMap<String, ReplicaIdentity>,
    authority: AuthorityStatus,
    authority_event_id: String,
}

/// The lock that serializes authority checks and transitions in one store.
pub struct ReplicaLock(File);

impl Drop for ReplicaLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Serialize every operation whose result depends on local replica state.
pub fn lock(store: &Store) -> Result<ReplicaLock> {
    let lock_dir = store.root.join("locks");
    create_private_dir_all(&lock_dir)?;
    let path = lock_dir.join("replica-state.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("cannot open replica-state lock {}", path.display()))?;
    let deadline = Instant::now() + STATE_LOCK_TIMEOUT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(ReplicaLock(file)),
            Err(std::fs::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    bail!(
                        "replica-state lock {} is busy; retry the command",
                        path.display()
                    );
                }
                thread::sleep(STATE_LOCK_RETRY);
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error)
                    .with_context(|| format!("cannot lock replica-state file {}", path.display()));
            }
        }
    }
}

pub fn id(store: &Store) -> ReplicaId {
    ReplicaId {
        schema: "arc-replica-id/1",
        repository_id: store.repository_id.clone(),
    }
}

/// Read the complete local replica view, including events imported from peers.
pub fn status(store: &Store) -> Result<ReplicaStatus> {
    let events = load_events(store)?;
    let snapshot = snapshot(&events, &store.repository_id)?;
    Ok(status_from_snapshot(store, snapshot))
}

fn status_from_snapshot(store: &Store, snapshot: Option<ReplicaSnapshot>) -> ReplicaStatus {
    match snapshot {
        Some(snapshot) => ReplicaStatus {
            schema: "arc-replica/1",
            repository_id: store.repository_id.clone(),
            project_id: Some(snapshot.project_id),
            peers: snapshot
                .members
                .values()
                .filter(|member| member.repository_id != store.repository_id)
                .cloned()
                .collect(),
            local: Some(snapshot.local),
            authority: Some(snapshot.authority),
        },
        None => ReplicaStatus {
            schema: "arc-replica/1",
            repository_id: store.repository_id.clone(),
            project_id: None,
            local: None,
            peers: Vec::new(),
            authority: None,
        },
    }
}

/// Render the same authority facts used by the JSON status surfaces.
pub fn render_status(status: &ReplicaStatus) {
    let Some(local) = &status.local else {
        println!("replica: unpaired (repository {})", status.repository_id);
        return;
    };
    println!(
        "replica: {} (repository {}, project {})",
        local.name,
        local.repository_id,
        status.project_id.as_deref().unwrap_or("unknown")
    );
    if status.peers.is_empty() {
        println!("  peers: none");
    } else {
        println!(
            "  peers: {}",
            status
                .peers
                .iter()
                .map(|peer| format!("{} ({})", peer.name, peer.repository_id))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let Some(authority) = &status.authority else {
        println!("  integration authority: none");
        return;
    };
    if let Some(holder) = &authority.holder {
        println!("  integration authority: held by {}", holder.name);
    } else if let Some(offer) = &authority.offer_in_flight {
        println!(
            "  integration authority: offer in flight from {} to {} ({})",
            offer.from.name, offer.to.name, offer.offer_id
        );
    } else {
        println!("  integration authority: none");
    }
    for contest in &authority.contested {
        println!(
            "  contested: {} acquired offer {} before {} reclaimed it; {} may have integrated while it believed it held authority",
            contest.acquired_by.name,
            contest.offer_id,
            contest.reclaimed_by.name,
            contest.acquired_by.name
        );
    }
    if let Some(reclaim) = &authority.last_reclaim {
        println!(
            "  last reclaim: {} from offer {} ({})",
            reclaim.reason, reclaim.offer_id, reclaim.from.name
        );
    }
}

/// Clear foreign checkout bindings from the operational state projection.
/// The originating event remains intact for provenance and export.
pub fn localize_change(
    local_repository_id: &str,
    events: &[Event],
    state: &mut crate::state::ChangeState,
) {
    let opener = events
        .iter()
        .find(|event| matches!(event.payload, Payload::ChangeOpened { .. }));
    if opener.is_some_and(|event| event.repository_id != local_repository_id) {
        state.worktree = None;
    }
}

pub fn init(ctx: &Ctx, store: &Store, name: &str) -> Result<()> {
    ids::validate_slug(name).context("replica name must use [a-z0-9-]")?;
    ctx.ensure_declared_actor(store)?;
    let _lock = lock(store)?;
    if !load_events(store)?.is_empty() {
        bail!("this store already belongs to a replica project; import its pairing record");
    }
    let project_id = ids::new_event_id();
    let identity = ReplicaIdentity {
        name: name.to_string(),
        repository_id: store.repository_id.clone(),
    };
    let event = make_event(
        ctx,
        store,
        &project_id,
        identity,
        ReplicaPayload::Initialized,
    );
    write_local_event(store, &event)?;
    println!("replica: {name}");
    println!("project: {project_id}");
    println!("integration authority: held by {name}");
    Ok(())
}

pub fn pair(ctx: &Ctx, store: &Store, name: &str, repository_id: &str) -> Result<()> {
    ids::validate_slug(name).context("replica name must use [a-z0-9-]")?;
    ids::validate_id_component(repository_id).context("invalid peer repository ID")?;
    ctx.ensure_declared_actor(store)?;
    let _lock = lock(store)?;
    let events = load_events(store)?;
    let current = snapshot(&events, &store.repository_id)?
        .context("initialize the first replica before pairing another store")?;
    if repository_id == store.repository_id {
        bail!("a store cannot pair with itself");
    }
    match current.members.get(repository_id) {
        Some(existing) if existing.name == name => {
            println!("already paired: {name} ({repository_id})");
            return Ok(());
        }
        Some(existing) => bail!(
            "repository {repository_id} is already paired as {:?}",
            existing.name
        ),
        None => {}
    }
    if let Some(existing) = current.members.values().find(|member| member.name == name) {
        bail!(
            "replica name {name:?} is already assigned to repository {}",
            existing.repository_id
        );
    }
    let event = make_event(
        ctx,
        store,
        &current.project_id,
        current.local,
        ReplicaPayload::Paired {
            peer: ReplicaIdentity {
                name: name.to_string(),
                repository_id: repository_id.to_string(),
            },
        },
    );
    write_local_event(store, &event)?;
    println!("paired: {name} ({repository_id})");
    println!("export the pairing record with `arc replica export`");
    Ok(())
}

pub fn export(store: &Store, output: &str) -> Result<()> {
    let events = load_events(store)?;
    let state = snapshot(&events, &store.repository_id)?
        .context("this store has no replica project to export")?;
    let mut events = events;
    events.sort_by(|left, right| left.event_id.cmp(&right.event_id));
    let mut bundle = ReplicaBundle {
        schema: BUNDLE_SCHEMA.to_string(),
        project_id: state.project_id,
        source_replica_id: store.repository_id.clone(),
        events,
        events_sha256: String::new(),
    };
    bundle.events_sha256 = events_digest(&bundle.events)?;
    let bytes = serde_json::to_vec_pretty(&bundle)?;
    if output == "-" {
        use std::io::Write as _;
        std::io::stdout().write_all(&bytes)?;
        eprintln!("events: {}", bundle.events.len());
        eprintln!("sha256: {}", bundle_digest(&bundle)?);
    } else {
        fs::write(output, &bytes)
            .with_context(|| format!("cannot write replica bundle {output}"))?;
        println!("events: {}", bundle.events.len());
        println!("sha256: {}", bundle_digest(&bundle)?);
        println!("output: {output}");
    }
    Ok(())
}

pub fn import(ctx: &Ctx, store: &Store, input: &str, dry_run: bool) -> Result<i32> {
    let bytes = if input == "-" {
        use std::io::Read as _;
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        bytes
    } else {
        fs::read(input).with_context(|| format!("cannot read replica bundle {input}"))?
    };
    let bundle = parse_bundle(&bytes)?;
    let digest = bundle_digest(&bundle)?;
    let import_path = imports_dir(store).join(format!("{digest}.json"));
    let _lock = lock(store)?;
    let known = load_events(store)?;
    let known_by_id = event_map(&known)?;
    let receipt_exists = import_path.is_file();
    let mut added = Vec::new();
    for event in &bundle.events {
        match known_by_id.get(&event.event_id) {
            Some(existing) if existing == event => {}
            Some(_) => bail!(
                "replica event {} conflicts with the local event; nothing was imported",
                event.event_id
            ),
            None => added.push(event.clone()),
        }
    }
    let mut candidate = known.clone();
    candidate.extend(added.iter().cloned());
    let before = snapshot(&known, &store.repository_id)?;
    let mut after = snapshot(&candidate, &store.repository_id)?;
    if before
        .as_ref()
        .is_some_and(|state| state.project_id != bundle.project_id)
    {
        bail!("replica bundle belongs to a different logical project; nothing was imported");
    }
    if !after
        .as_ref()
        .is_some_and(|state| state.members.contains_key(&bundle.source_replica_id))
    {
        bail!("replica bundle source is not a member of its project; nothing was imported");
    }
    let local = after
        .as_ref()
        .and_then(|state| state.members.get(&store.repository_id))
        .cloned()
        .context("replica bundle does not name this local repository")?;
    let accepted_offer = after.as_ref().and_then(|state| {
        state
            .authority
            .offer_in_flight
            .as_ref()
            .filter(|offer| offer.to.repository_id == store.repository_id)
            .map(|offer| offer.offer_id.clone())
    });
    let mut acquired = None;
    if let Some(offer_id) = accepted_offer {
        let already_acquired = candidate.iter().any(|event| {
            matches!(
                &event.payload,
                ReplicaPayload::AuthorityAcquired { offer_id: existing }
                    if existing == &offer_id
            )
        });
        if !already_acquired {
            let project_id = after
                .as_ref()
                .map(|state| state.project_id.as_str())
                .context("replica project disappeared during import planning")?;
            let event = make_event(
                ctx,
                store,
                project_id,
                local.clone(),
                ReplicaPayload::AuthorityAcquired { offer_id },
            );
            candidate.push(event.clone());
            added.push(event);
            acquired = Some(true);
            after = snapshot(&candidate, &store.repository_id)?;
        }
    }
    if dry_run {
        print_import_report(
            &bundle,
            &digest,
            &added,
            receipt_exists,
            after.as_ref(),
            true,
        );
        return Ok(0);
    }
    ctx.ensure_declared_actor(store)?;
    if receipt_exists {
        println!("source replica: {}", bundle.source_replica_id);
        println!("bundle sha256: {digest}");
        println!("events: 0 imported (receipt already recorded); no changes");
        return Ok(0);
    }
    let batch = ImportBatch {
        schema: IMPORT_SCHEMA.to_string(),
        source_replica_id: bundle.source_replica_id.clone(),
        bundle_sha256: digest.clone(),
        imported_at: Utc::now(),
        events: added.clone(),
    };
    let mut batch_bytes = serde_json::to_vec_pretty(&batch)?;
    batch_bytes.push(b'\n');
    create_private_dir_all(
        import_path
            .parent()
            .context("replica import has no parent")?,
    )?;
    write_exclusive(&import_path, &batch_bytes)
        .with_context(|| format!("replica import receipt {digest} already exists"))?;
    print_import_report(&bundle, &digest, &added, false, after.as_ref(), false);
    if acquired.is_some() {
        println!("integration authority: acquired by {}", local.name);
    }
    Ok(0)
}

fn print_import_report(
    bundle: &ReplicaBundle,
    digest: &str,
    added: &[ReplicaEvent],
    receipt_exists: bool,
    after: Option<&ReplicaSnapshot>,
    dry_run: bool,
) {
    println!("source replica: {}", bundle.source_replica_id);
    println!("bundle sha256: {digest}");
    if dry_run {
        println!("dry-run: would import {} event(s)", added.len());
    } else {
        println!("events: {} imported", added.len());
    }
    if receipt_exists {
        println!("receipt: already recorded");
    } else if dry_run {
        println!("receipt: would record source replica and bundle digest");
    } else {
        println!("receipt: recorded");
    }
    if let Some(state) = after {
        if let Some(holder) = &state.authority.holder {
            println!("integration authority: held by {}", holder.name);
        } else if let Some(offer) = &state.authority.offer_in_flight {
            println!(
                "integration authority: offer in flight to {}",
                offer.to.name
            );
        }
    }
}

pub fn offer(ctx: &Ctx, store: &Store, recipient: &str) -> Result<()> {
    ctx.ensure_declared_actor(store)?;
    let _lock = lock(store)?;
    let events = load_events(store)?;
    let current = snapshot(&events, &store.repository_id)?
        .context("initialize or import a pairing record before offering authority")?;
    let to = current
        .members
        .values()
        .find(|member| member.name == recipient)
        .cloned()
        .with_context(|| format!("replica {recipient:?} is not paired with this project"))?;
    if to.repository_id == store.repository_id {
        bail!("a replica cannot offer integration authority to itself");
    }
    let Some(holder) = &current.authority.holder else {
        bail!("integration authority is not held locally; it cannot be offered");
    };
    if holder.repository_id != store.repository_id {
        bail!(
            "integration authority is held by {}; this replica cannot offer it",
            holder.name
        );
    }
    let parent_event_id = active_grant(&events, &current.project_id, &store.repository_id)?;
    let event_id = ids::new_event_id();
    let event = make_event_with_id(
        ctx,
        store,
        &current.project_id,
        current.local,
        event_id.clone(),
        ReplicaPayload::AuthorityOffered {
            offer_id: event_id.clone(),
            to: to.clone(),
            parent_event_id,
        },
    );
    write_local_event(store, &event)?;
    println!("authority offer: {event_id}");
    println!("from: {}", holder.name);
    println!("to: {}", to.name);
    println!("integration authority: none; offer in flight");
    println!("export the offer with `arc replica export`");
    Ok(())
}

pub fn reclaim(ctx: &Ctx, store: &Store, reason: &str) -> Result<()> {
    if reason.trim().is_empty() {
        bail!("a reason is required to reclaim integration authority");
    }
    ctx.ensure_declared_actor(store)?;
    let _lock = lock(store)?;
    let events = load_events(store)?;
    let current =
        snapshot(&events, &store.repository_id)?.context("this store has no replica project")?;
    let offer = current
        .authority
        .offer_in_flight
        .as_ref()
        .filter(|offer| offer.from.repository_id == store.repository_id)
        .cloned()
        .context("this replica has no integration authority offer in flight to reclaim")?;
    let event = make_event(
        ctx,
        store,
        &current.project_id,
        current.local,
        ReplicaPayload::AuthorityReclaimed {
            offer_id: offer.offer_id.clone(),
            reason: reason.to_string(),
        },
    );
    write_local_event(store, &event)?;
    println!("authority offer reclaimed: {}", offer.offer_id);
    println!("reason: {reason}");
    println!("integration authority: held by {}", offer.from.name);
    println!("export the reclaim with `arc replica export`");
    Ok(())
}

/// Return an integration refusal when a paired local store lacks authority.
pub fn integration_refusal(store: &Store) -> Result<Option<String>> {
    let status = status(store)?;
    let Some(local) = status.local else {
        return Ok(None);
    };
    let Some(authority) = status.authority else {
        return Ok(None);
    };
    if authority
        .holder
        .as_ref()
        .is_some_and(|holder| holder.repository_id == store.repository_id)
    {
        return Ok(None);
    }
    let refusal = if let Some(holder) = authority.holder {
        format!(
            "replica {} does not hold integration authority; holder is {}",
            local.name, holder.name
        )
    } else if let Some(offer) = authority.offer_in_flight {
        format!(
            "replica {} does not hold integration authority; offer is in flight from {} to {}",
            local.name, offer.from.name, offer.to.name
        )
    } else {
        format!("replica {} does not hold integration authority", local.name)
    };
    Ok(Some(refusal))
}

/// Detect a live local claim competing with a newly imported peer claim.
pub fn live_claim_contest(
    store: &Store,
    change_id: &str,
    incoming_events: &[Event],
    new_event_ids: &BTreeSet<String>,
) -> Result<Option<ClaimContest>> {
    if !store
        .list_change_ids()?
        .iter()
        .any(|existing| existing == change_id)
    {
        return Ok(None);
    }
    let replica_status = status(store)?;
    let Some(local_replica) = replica_status.local else {
        return Ok(None);
    };
    if replica_status.peers.is_empty() {
        return Ok(None);
    }
    let local_events = store.load_events(change_id)?;
    let local_state = store.state(change_id)?;
    let Some(local_claim) = local_state.claim.as_ref() else {
        return Ok(None);
    };
    if !crate::state::claim_timing_at(local_claim, Utc::now()).active {
        return Ok(None);
    }
    let local_claim_is_local = local_events.iter().any(|event| {
        event.repository_id == store.repository_id
            && matches!(
                &event.payload,
                Payload::ClaimSet { claim_id, .. } if claim_id == &local_claim.claim_id
            )
    });
    if !local_claim_is_local {
        return Ok(None);
    }

    let mut incoming_events = incoming_events.to_vec();
    incoming_events.sort_by(|left, right| left.event_id.cmp(&right.event_id));
    let incoming_state = crate::state::reduce(&incoming_events)?;
    let Some(incoming_claim) = incoming_state.claim.as_ref() else {
        return Ok(None);
    };
    if !crate::state::claim_timing_at(incoming_claim, Utc::now()).active
        || incoming_claim.claim_id == local_claim.claim_id
    {
        return Ok(None);
    }
    let incoming_claim_event = incoming_events.iter().rev().find(|event| {
        new_event_ids.contains(&event.event_id)
            && matches!(
                &event.payload,
                Payload::ClaimSet { claim_id, .. } if claim_id == &incoming_claim.claim_id
            )
    });
    let Some(incoming_claim_event) = incoming_claim_event else {
        return Ok(None);
    };
    if incoming_claim_event.repository_id == store.repository_id {
        return Ok(None);
    }
    let incoming_replica = replica_status
        .peers
        .iter()
        .find(|peer| peer.repository_id == incoming_claim_event.repository_id)
        .map(|peer| peer.name.clone())
        .unwrap_or_else(|| incoming_claim_event.repository_id.clone());
    Ok(Some(ClaimContest {
        local_replica: local_replica.name,
        local_actor: local_claim.owner.actor.clone(),
        incoming_replica,
        incoming_actor: incoming_claim.owner.actor.clone(),
    }))
}

fn make_event(
    ctx: &Ctx,
    store: &Store,
    project_id: &str,
    actor: ReplicaIdentity,
    payload: ReplicaPayload,
) -> ReplicaEvent {
    make_event_with_id(ctx, store, project_id, actor, ids::new_event_id(), payload)
}

fn make_event_with_id(
    ctx: &Ctx,
    store: &Store,
    project_id: &str,
    actor: ReplicaIdentity,
    event_id: String,
    payload: ReplicaPayload,
) -> ReplicaEvent {
    ReplicaEvent {
        schema: EVENT_SCHEMA.to_string(),
        event_id,
        project_id: project_id.to_string(),
        repository_id: store.repository_id.clone(),
        actor,
        recorded_by: ctx.actor.clone(),
        harness: ctx.harness.clone(),
        session: ctx.session.clone(),
        model: ctx.model.clone(),
        created_at: Utc::now(),
        payload,
    }
}

fn active_grant(events: &[ReplicaEvent], project_id: &str, repository_id: &str) -> Result<String> {
    let state = snapshot(events, repository_id)?.context("this store has no replica project")?;
    if state.project_id != project_id {
        bail!("replica project ID does not match the active authority state");
    }
    if state.local.repository_id != repository_id {
        bail!("local replica identity does not match the store repository ID");
    }
    if state
        .authority
        .holder
        .as_ref()
        .is_none_or(|holder| holder.repository_id != repository_id)
    {
        bail!("this replica does not hold integration authority");
    }
    Ok(state.authority_event_id)
}

fn snapshot(events: &[ReplicaEvent], local_repository_id: &str) -> Result<Option<ReplicaSnapshot>> {
    if events.is_empty() {
        return Ok(None);
    }
    let mut by_id = BTreeMap::new();
    for event in events {
        validate_event(event)?;
        match by_id.insert(event.event_id.as_str(), event) {
            Some(existing) if existing != event => {
                bail!("replica event {} has conflicting contents", event.event_id)
            }
            Some(_) => bail!("replica event {} is duplicated", event.event_id),
            None => {}
        }
    }
    let projects = events
        .iter()
        .map(|event| event.project_id.as_str())
        .collect::<BTreeSet<_>>();
    if projects.len() != 1 {
        bail!("replica store contains events from different logical projects");
    }
    let project_id = events[0].project_id.clone();
    let initialized = events
        .iter()
        .filter(|event| matches!(event.payload, ReplicaPayload::Initialized))
        .collect::<Vec<_>>();
    let [initial] = initialized.as_slice() else {
        bail!("replica project must have exactly one initial identity event");
    };
    let mut members = BTreeMap::<String, ReplicaIdentity>::new();
    add_member(&mut members, initial.actor.clone())?;
    for event in events {
        if let ReplicaPayload::Paired { peer } = &event.payload {
            add_member(&mut members, event.actor.clone())?;
            add_member(&mut members, peer.clone())?;
        }
    }
    let mut reachable = BTreeSet::from([initial.actor.repository_id.clone()]);
    loop {
        let before = reachable.len();
        for event in events {
            if let ReplicaPayload::Paired { peer } = &event.payload {
                if reachable.contains(&event.actor.repository_id) {
                    reachable.insert(peer.repository_id.clone());
                }
            }
        }
        if reachable.len() == before {
            break;
        }
    }
    if reachable.len() != members.len() {
        bail!("replica pairing records include a member not paired from the initial replica");
    }
    for event in events {
        match &event.payload {
            ReplicaPayload::Initialized | ReplicaPayload::Paired { .. } => {}
            ReplicaPayload::AuthorityOffered { to, .. } => {
                if members.get(&event.actor.repository_id) != Some(&event.actor) {
                    bail!(
                        "authority offer {} is authored by an unknown replica",
                        event.event_id
                    );
                }
                if members.get(&to.repository_id) != Some(to) {
                    bail!(
                        "authority offer {} names an unpaired recipient",
                        event.event_id
                    );
                }
            }
            ReplicaPayload::AuthorityAcquired { .. }
            | ReplicaPayload::AuthorityReclaimed { .. } => {
                if members.get(&event.actor.repository_id) != Some(&event.actor) {
                    bail!(
                        "authority event {} is authored by an unknown replica",
                        event.event_id
                    );
                }
            }
        }
    }
    if members.get(&initial.actor.repository_id) != Some(&initial.actor) {
        bail!("initial replica identity conflicts with a pairing record");
    }
    let local = members.get(local_repository_id).cloned();
    let local = local.context("this repository is not named in the replica pairing record")?;
    let (authority, authority_event_id) = authority_state(events, initial, &members)?;
    Ok(Some(ReplicaSnapshot {
        project_id,
        local,
        members,
        authority,
        authority_event_id,
    }))
}

fn add_member(
    members: &mut BTreeMap<String, ReplicaIdentity>,
    identity: ReplicaIdentity,
) -> Result<()> {
    validate_identity(&identity)?;
    if let Some(existing) = members.get(&identity.repository_id) {
        if existing != &identity {
            bail!(
                "repository {} has conflicting replica names {:?} and {:?}",
                identity.repository_id,
                existing.name,
                identity.name
            );
        }
        return Ok(());
    }
    if let Some(existing) = members
        .values()
        .find(|existing| existing.name == identity.name)
    {
        bail!(
            "replica name {:?} is assigned to repositories {} and {}",
            identity.name,
            existing.repository_id,
            identity.repository_id
        );
    }
    members.insert(identity.repository_id.clone(), identity);
    Ok(())
}

fn authority_state(
    events: &[ReplicaEvent],
    initial: &ReplicaEvent,
    members: &BTreeMap<String, ReplicaIdentity>,
) -> Result<(AuthorityStatus, String)> {
    let offers = events
        .iter()
        .filter_map(|event| match &event.payload {
            ReplicaPayload::AuthorityOffered { offer_id, .. } => Some((offer_id.as_str(), event)),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    if offers.len()
        != events
            .iter()
            .filter(|event| matches!(event.payload, ReplicaPayload::AuthorityOffered { .. }))
            .count()
    {
        bail!("replica project contains duplicate authority offer IDs");
    }
    let mut acquires = BTreeMap::<&str, &ReplicaEvent>::new();
    let mut reclaims = BTreeMap::<&str, &ReplicaEvent>::new();
    for event in events {
        match &event.payload {
            ReplicaPayload::AuthorityAcquired { offer_id } => {
                let offer = offers.get(offer_id.as_str()).with_context(|| {
                    format!("authority acquisition names missing offer {offer_id}")
                })?;
                let to = match &offer.payload {
                    ReplicaPayload::AuthorityOffered { to, .. } => to,
                    _ => unreachable!("offers contains only authority offer events"),
                };
                if to.repository_id != event.actor.repository_id {
                    bail!("authority offer {offer_id} was acquired by an unnamed replica");
                }
                if acquires.insert(offer_id, event).is_some() {
                    bail!("authority offer {offer_id} was acquired more than once");
                }
            }
            ReplicaPayload::AuthorityReclaimed { offer_id, reason } => {
                let offer = offers
                    .get(offer_id.as_str())
                    .with_context(|| format!("authority reclaim names missing offer {offer_id}"))?;
                if event.actor.repository_id != offer.actor.repository_id {
                    bail!("authority offer {offer_id} was reclaimed by a non-offering replica");
                }
                if reason.trim().is_empty() {
                    bail!("authority offer {offer_id} has a reclaim without a reason");
                }
                if reclaims.insert(offer_id, event).is_some() {
                    bail!("authority offer {offer_id} was reclaimed more than once");
                }
            }
            _ => {}
        }
    }
    let mut offers_by_parent = BTreeMap::<&str, &ReplicaEvent>::new();
    for (offer_id, event) in &offers {
        let (payload_id, parent_id) = match &event.payload {
            ReplicaPayload::AuthorityOffered {
                offer_id,
                parent_event_id,
                ..
            } => (offer_id, parent_event_id),
            _ => unreachable!("offers contains only authority offer events"),
        };
        if offers_by_parent
            .insert(parent_id.as_str(), *event)
            .is_some()
        {
            bail!("replica project contains competing offers from one authority state");
        }
        if payload_id != offer_id {
            bail!("authority offer ID does not match its event ID");
        }
    }
    let mut grants = BTreeMap::<String, String>::new();
    grants.insert(
        initial.event_id.clone(),
        initial.actor.repository_id.clone(),
    );
    let mut pending = offers.clone();
    let mut offer_grants = BTreeMap::<String, String>::new();
    loop {
        let ready = pending
            .iter()
            .filter_map(|(offer_id, event)| {
                let parent_id = match &event.payload {
                    ReplicaPayload::AuthorityOffered {
                        parent_event_id, ..
                    } => parent_event_id.as_str(),
                    _ => unreachable!("pending contains only authority offer events"),
                };
                grants
                    .get(parent_id)
                    .map(|holder| ((*offer_id).to_string(), *event, holder.clone()))
            })
            .collect::<Vec<_>>();
        if ready.is_empty() {
            break;
        }
        for (offer_id, event, holder_id) in ready {
            if event.actor.repository_id != holder_id {
                bail!("authority offer {offer_id} was not made by its holder");
            }
            if let ReplicaPayload::AuthorityOffered {
                parent_event_id, ..
            } = &event.payload
            {
                offer_grants.insert(offer_id.clone(), parent_event_id.clone());
            }
            pending.remove(offer_id.as_str());
            if let Some(acquired) = acquires.get(offer_id.as_str()) {
                grants.insert(
                    acquired.event_id.clone(),
                    acquired.actor.repository_id.clone(),
                );
            }
            if let Some(reclaimed) = reclaims.get(offer_id.as_str()) {
                grants.insert(
                    reclaimed.event_id.clone(),
                    reclaimed.actor.repository_id.clone(),
                );
            }
        }
    }
    if let Some((offer_id, _)) = pending.iter().next() {
        bail!("authority offer {offer_id} names an unknown authority event");
    }
    for event in events {
        match &event.payload {
            ReplicaPayload::AuthorityAcquired { offer_id }
            | ReplicaPayload::AuthorityReclaimed { offer_id, .. }
                if !offer_grants.contains_key(offer_id) =>
            {
                bail!("authority event names an unreachable offer {offer_id}");
            }
            _ => {}
        }
    }
    let mut holder_id = initial.actor.repository_id.clone();
    let mut grant_id = initial.event_id.clone();
    let mut last_reclaim = None;
    let mut contested = Vec::new();
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(grant_id.clone()) {
            bail!("replica authority history contains a cycle");
        }
        let Some(offer) = offers_by_parent.get(grant_id.as_str()) else {
            return Ok((
                AuthorityStatus {
                    holder: members.get(&holder_id).cloned(),
                    offer_in_flight: None,
                    last_reclaim,
                    contested,
                },
                grant_id,
            ));
        };
        let offer_id = match &offer.payload {
            ReplicaPayload::AuthorityOffered { offer_id, .. } => offer_id,
            _ => unreachable!("offers_by_parent contains only authority offers"),
        };
        if let Some(reclaimed) = reclaims.get(offer_id.as_str()) {
            let reason = match &reclaimed.payload {
                ReplicaPayload::AuthorityReclaimed { reason, .. } => reason.clone(),
                _ => unreachable!("reclaims contains only reclaim events"),
            };
            last_reclaim = Some(AuthorityReclaim {
                offer_id: offer_id.clone(),
                from: offer.actor.clone(),
                reason,
            });
            if let Some(acquired) = acquires.get(offer_id.as_str()) {
                contested.push(AuthorityContest {
                    offer_id: offer_id.clone(),
                    acquired_by: acquired.actor.clone(),
                    reclaimed_by: reclaimed.actor.clone(),
                });
            }
            holder_id = offer.actor.repository_id.clone();
            grant_id = reclaimed.event_id.clone();
            continue;
        }
        if let Some(acquired) = acquires.get(offer_id.as_str()) {
            holder_id = acquired.actor.repository_id.clone();
            grant_id = acquired.event_id.clone();
            continue;
        }
        let to = match &offer.payload {
            ReplicaPayload::AuthorityOffered { to, .. } => to.clone(),
            _ => unreachable!("offers_by_parent contains only authority offers"),
        };
        return Ok((
            AuthorityStatus {
                holder: None,
                offer_in_flight: Some(AuthorityOffer {
                    offer_id: offer_id.clone(),
                    from: offer.actor.clone(),
                    to,
                }),
                last_reclaim,
                contested,
            },
            grant_id,
        ));
    }
}

fn validate_event(event: &ReplicaEvent) -> Result<()> {
    if event.schema != EVENT_SCHEMA {
        bail!("unsupported replica event schema {:?}", event.schema);
    }
    ids::validate_id_component(&event.event_id)?;
    ids::validate_id_component(&event.project_id)?;
    ids::validate_id_component(&event.repository_id)?;
    validate_identity(&event.actor)?;
    if event.repository_id != event.actor.repository_id {
        bail!(
            "replica event {} has a mismatched repository identity",
            event.event_id
        );
    }
    match &event.payload {
        ReplicaPayload::Initialized => {}
        ReplicaPayload::Paired { peer } => validate_identity(peer)?,
        ReplicaPayload::AuthorityOffered {
            offer_id,
            to,
            parent_event_id,
        } => {
            ids::validate_id_component(offer_id)?;
            ids::validate_id_component(parent_event_id)?;
            validate_identity(to)?;
            if to.repository_id == event.actor.repository_id {
                bail!("authority offer {} names its own replica", event.event_id);
            }
        }
        ReplicaPayload::AuthorityAcquired { offer_id } => ids::validate_id_component(offer_id)?,
        ReplicaPayload::AuthorityReclaimed { offer_id, reason } => {
            ids::validate_id_component(offer_id)?;
            if reason.trim().is_empty() {
                bail!("authority reclaim reason cannot be empty");
            }
        }
    }
    Ok(())
}

fn validate_identity(identity: &ReplicaIdentity) -> Result<()> {
    ids::validate_slug(&identity.name).context("replica names must use [a-z0-9-]")?;
    ids::validate_id_component(&identity.repository_id).context("invalid replica repository ID")?;
    Ok(())
}

fn parse_bundle(bytes: &[u8]) -> Result<ReplicaBundle> {
    let bundle: ReplicaBundle =
        serde_json::from_slice(bytes).context("malformed replica bundle")?;
    if bundle.schema != BUNDLE_SCHEMA {
        bail!("unsupported replica bundle schema {:?}", bundle.schema);
    }
    ids::validate_id_component(&bundle.project_id)?;
    ids::validate_id_component(&bundle.source_replica_id)?;
    for event in &bundle.events {
        validate_event(event)?;
        if event.project_id != bundle.project_id {
            bail!("replica bundle carries an event from another project");
        }
    }
    if events_digest(&bundle.events)? != bundle.events_sha256 {
        bail!("replica bundle event checksum does not match");
    }
    let map = event_map(&bundle.events)?;
    if map.len() != bundle.events.len() {
        bail!("replica bundle carries duplicate event IDs");
    }
    Ok(bundle)
}

fn event_map(events: &[ReplicaEvent]) -> Result<BTreeMap<String, ReplicaEvent>> {
    let mut out = BTreeMap::new();
    for event in events {
        validate_event(event)?;
        if let Some(existing) = out.get(&event.event_id) {
            if existing != event {
                bail!("replica event {} has conflicting contents", event.event_id);
            }
            bail!("replica event {} appears more than once", event.event_id);
        }
        out.insert(event.event_id.clone(), event.clone());
    }
    Ok(out)
}

fn events_digest(events: &[ReplicaEvent]) -> Result<String> {
    let mut hasher = Sha256::new();
    for event in events {
        hasher.update(serde_json::to_vec(event)?);
        hasher.update(b"\n");
    }
    Ok(hex::encode(hasher.finalize()))
}

fn bundle_digest(bundle: &ReplicaBundle) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(bundle)?)))
}

fn load_events(store: &Store) -> Result<Vec<ReplicaEvent>> {
    let root = replica_root(store);
    let mut events = BTreeMap::<String, ReplicaEvent>::new();
    let event_dir = root.join("events");
    if event_dir.is_dir() {
        for path in json_files(&event_dir)? {
            let event: ReplicaEvent = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("malformed replica event {}", path.display()))?;
            insert_event(&mut events, event)?;
        }
    }
    let imported_dir = root.join("imports");
    if imported_dir.is_dir() {
        for path in json_files(&imported_dir)? {
            let batch: ImportBatch = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("malformed replica import receipt {}", path.display()))?;
            if batch.schema != IMPORT_SCHEMA {
                bail!(
                    "unsupported replica import receipt schema in {}",
                    path.display()
                );
            }
            ids::validate_id_component(&batch.source_replica_id)?;
            if path.file_stem().and_then(|name| name.to_str()) != Some(batch.bundle_sha256.as_str())
            {
                bail!("replica import receipt filename does not match its digest");
            }
            for event in batch.events {
                insert_event(&mut events, event)?;
            }
        }
    }
    Ok(events.into_values().collect())
}

fn insert_event(events: &mut BTreeMap<String, ReplicaEvent>, event: ReplicaEvent) -> Result<()> {
    validate_event(&event)?;
    if let Some(existing) = events.get(&event.event_id) {
        if existing != &event {
            bail!("replica event {} has conflicting contents", event.event_id);
        }
        bail!("replica event {} is stored more than once", event.event_id);
    }
    events.insert(event.event_id.clone(), event);
    Ok(())
}

fn json_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(dir)
        .with_context(|| format!("cannot read replica directory {}", dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths.sort();
    Ok(paths)
}

fn replica_root(store: &Store) -> PathBuf {
    store.root.join("replicas")
}

fn imports_dir(store: &Store) -> PathBuf {
    replica_root(store).join("imports")
}

fn write_local_event(store: &Store, event: &ReplicaEvent) -> Result<()> {
    let dir = replica_root(store).join("events");
    create_private_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", event.event_id));
    let mut bytes = serde_json::to_vec_pretty(event)?;
    bytes.push(b'\n');
    write_exclusive(&path, &bytes)
        .with_context(|| format!("replica event {} already exists", event.event_id))
}

fn create_private_dir_all(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .or_else(|error| if path.is_dir() { Ok(()) } else { Err(error) })
        .with_context(|| format!("cannot create replica directory {}", path.display()))
}

/// Publish one complete JSON record without replacing an existing fact.
fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<()> {
    let file_name = path
        .file_name()
        .context("replica event path has no file name")?
        .to_string_lossy();
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", ids::new_event_id()));
    let publish = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("cannot create {}", temporary.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::hard_link(&temporary, path)
            .with_context(|| format!("cannot create {}", path.display()))?;
        File::open(path.parent().context("replica event path has no parent")?)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    publish
}
