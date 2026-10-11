use crate::gitio;
use crate::ids;
use crate::import_authorization::PrivilegeScope;
use crate::model::{ActorSource, Event, Payload};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

// A known opening ID locates an immutable event directly when the directory
// becomes unlistable. The event bytes are still validated before any append.
static OPENING_IDS: std::sync::Mutex<BTreeMap<(PathBuf, String), String>> =
    std::sync::Mutex::new(BTreeMap::new());

#[cfg(test)]
thread_local! {
    static EVENT_ID_CANDIDATE: std::cell::Cell<Option<ulid::Ulid>> = const { std::cell::Cell::new(None) };
    static LOCAL_PUBLICATION_PAUSE: std::cell::RefCell<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>> = const { std::cell::RefCell::new(None) };
}

// Candidate payload variants and wire names share one format barrier.
macro_rules! candidate_format_barrier {
    ($($variant:ident => $wire:literal),+ $(,)?) => {
        fn is_candidate_payload(payload: &Payload) -> bool {
            matches!(payload, $(Payload::$variant { .. })|+)
        }

        #[cfg(test)]
        fn is_candidate_event_type(event_type: &str) -> bool {
            matches!(event_type, $($wire)|+)
        }
    };
}

candidate_format_barrier! {
    CandidateRegistered => "candidate-registered",
    CandidateJudged => "candidate-judged",
    CandidateRetired => "candidate-retired",
    CandidateEvidenceReceived => "candidate-evidence-received",
    CandidateVerified => "candidate-verified",
    CandidateSelected => "candidate-selected",
    CandidatePromoted => "candidate-promoted",
}

const LOCK_TIMEOUT: Duration = Duration::from_secs(1);
// The target lock is legitimately held across a real merge, so waiters get a
// budget sized to an integration rather than to a state append.
const TARGET_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_RETRY: Duration = Duration::from_millis(10);

#[derive(Debug, Serialize, Deserialize)]
struct StoreConfig {
    schema_version: u32,
    repository_id: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// The machine-local operational store: one directory of append-only
/// event files per change, shared by every worktree of the repository.
pub struct Store {
    pub operator_policy_path: Option<PathBuf>,
    pub root: PathBuf,
    pub repository_id: String,
    /// Whether this repository requires every writer to declare itself, read
    /// once from the invoking checkout when the store was opened. This guard
    /// covers repository-wide events that have no change target; integration
    /// also checks the change target's policy before merging.
    ///
    /// Reading it per append would let a command's own merge change the rule
    /// it is being judged by: `integrate` can bring in a commit that enables
    /// the policy, and the closure event would then be refused by a rule that
    /// did not exist when the merge was authorised.
    pub require_declared_actor: bool,
    /// Files that enabled the declared-actor requirement.
    pub require_declared_actor_sources: Vec<String>,
    /// Opened for reading only: the append policy was never parsed, so this
    /// handle refuses every append.
    pub read_only: bool,
}

/// One read of the ledger. The import batches, the recorded rewrites and the
/// privilege scope are each loaded or derived at most once, the first time the
/// read needs them, and shared by every change it reads. A read of N changes
/// then costs N changes' events, not N times the ledger.
///
/// The handle describes the ledger as it stood when it was opened: a reader
/// that writes opens a new one afterwards.
pub struct LedgerRead<'a> {
    store: &'a Store,
    batches: Vec<ImportBatch>,
    scope: std::cell::OnceCell<Arc<PrivilegeScope>>,
    rewrites: std::cell::OnceCell<crate::rewrite::RewriteMap>,
}

impl LedgerRead<'_> {
    pub fn change_ids(&self) -> Result<Vec<String>> {
        self.store.list_change_ids_in(&self.batches)
    }

    pub fn events(&self, change_id: &str) -> Result<Vec<Event>> {
        self.store.load_events_in(change_id, &self.batches)
    }

    pub fn rewrites(&self) -> Result<&crate::rewrite::RewriteMap> {
        if let Some(held) = self.rewrites.get() {
            return Ok(held);
        }
        let loaded = self.store.rewrites_in(&self.batches)?;
        Ok(self.rewrites.get_or_init(|| loaded))
    }

    /// The scope, derived when first asked for. A read that never asks, as one
    /// over an empty ledger does not, never derives it, and one that cannot
    /// derive it fails at each change that asks.
    pub fn scope(&self) -> Result<&Arc<PrivilegeScope>> {
        if let Some(held) = self.scope.get() {
            return Ok(held);
        }
        let derived = self.store.privilege_scope()?;
        Ok(self.scope.get_or_init(|| derived))
    }

    /// A change's events reduced under this read's rewrites and scope.
    pub fn project(&self, events: &[Event]) -> Result<crate::state::ChangeState> {
        self.store
            .project_state_in(self.scope()?, events, self.rewrites()?)
    }

    pub fn state(&self, change_id: &str) -> Result<crate::state::ChangeState> {
        let events = self.events(change_id)?;
        self.rewrites()?;
        self.project(&events)
    }
}

/// A process-scoped transition guard. The lock file is intentionally
/// persistent: the OS releases the advisory lock when this handle is dropped
/// or the process exits, so a crash cannot leave a stale ownership marker.
pub struct TransitionLock(File);

/// A local fact without an issued identity. Only publication or a durable
/// provisional intent can assign its event ID.
pub(crate) struct LocalEvent(Event);

impl LocalEvent {
    pub(crate) fn draft(event: Event) -> Self {
        Self(event)
    }

    /// A speculative replay places the draft after the held history without
    /// issuing an identity or recording anything.
    pub(crate) fn preview_after(&self, events: &[Event]) -> Result<Event> {
        let mut preview = self.0.clone();
        let latest = events.iter().map(|event| event.event_id.as_str()).max();
        preview.event_id = match latest {
            Some(id) => ids::after_floor(ulid::Ulid::new(), id.parse::<ulid::Ulid>()?)
                .context("no event ID remains for a speculative replay")?
                .to_string(),
            None => ulid::Ulid::new().to_string(),
        };
        Ok(preview)
    }

    pub(crate) fn into_event(self) -> Event {
        self.0
    }
}

impl std::ops::Deref for LocalEvent {
    type Target = Event;
    fn deref(&self) -> &Event {
        &self.0
    }
}

impl std::ops::DerefMut for LocalEvent {
    fn deref_mut(&mut self) -> &mut Event {
        &mut self.0
    }
}

impl Drop for TransitionLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportBatch {
    pub schema: String,
    pub change_id: String,
    pub events: Vec<serde_json::Value>,
    pub repository_events: Vec<serde_json::Value>,
    pub authorization: Option<crate::import_authorization::ImportAuthorization>,
}

/// Only published JSON records participate in ledger reads.
pub fn import_batches(root: &Path) -> Result<Vec<ImportBatch>> {
    Ok(import_batches_with_paths(root)?
        .into_iter()
        .map(|(_, batch)| batch)
        .collect())
}

fn import_batches_with_paths(root: &Path) -> Result<Vec<(PathBuf, ImportBatch)>> {
    let dir = root.join("imports");
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", dir.display())),
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths.sort();
    let mut batches = Vec::new();
    for path in paths {
        let batch: ImportBatch = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("malformed import batch {}", path.display()))?;
        if batch.schema != "arc-import-batch/1" {
            bail!("unsupported import batch schema {}", batch.schema);
        }
        ids::validate_id_component(&batch.change_id)?;
        for value in &batch.events {
            if value["change_id"].as_str() != Some(batch.change_id.as_str()) {
                bail!(
                    "batch event {} belongs to another change",
                    value["event_id"]
                );
            }
        }
        if let Some(receipt) = &batch.authorization {
            let values: Vec<_> = batch
                .events
                .iter()
                .cloned()
                .map(|value| ("change".into(), value))
                .chain(
                    batch
                        .repository_events
                        .iter()
                        .cloned()
                        .map(|value| ("repository".into(), value)),
                )
                .collect();
            crate::import_authorization::validate_receipt_events(receipt, &values)?;
        }
        batches.push((path, batch));
    }
    Ok(batches)
}

fn event_file_stems(dir: &Path) -> Result<BTreeSet<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", dir.display())),
    };
    let mut stems = BTreeSet::new();
    for entry in entries {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".json") {
            stems.insert(stem.to_string());
        }
    }
    Ok(stems)
}

fn typed_repository_events(raw: Vec<(String, serde_json::Value)>) -> Result<Vec<Event>> {
    raw.into_iter()
        .filter_map(|(_, value)| match serde_json::from_value::<Event>(value) {
            Ok(event) if matches!(event.payload, Payload::Unknown) => None,
            other => Some(other.context("malformed repository event")),
        })
        .collect()
}

fn typed_events(raw: Vec<(String, serde_json::Value)>) -> Result<Vec<Event>> {
    raw.into_iter()
        .filter_map(|(_, value)| match serde_json::from_value::<Event>(value) {
            Ok(event) if matches!(event.payload, Payload::Unknown) => None,
            other => Some(other.context("malformed event file in change ledger")),
        })
        .collect()
}

fn merge_raw(
    events: &mut Vec<(String, serde_json::Value)>,
    incoming: impl IntoIterator<Item = serde_json::Value>,
    seen: &BTreeSet<String>,
) -> Result<()> {
    for value in incoming {
        let id = value
            .get("event_id")
            .and_then(serde_json::Value::as_str)
            .context("imported event has no ID")?
            .to_string();
        ids::validate_id_component(&id)?;
        if seen.contains(&id) {
            continue;
        }
        if let Some((_, existing)) = events.iter().find(|(held, _)| held == &id) {
            if existing != &value {
                bail!("event {id} has conflicting stored contents");
            }
        } else {
            events.push((id, value));
        }
    }
    events.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(())
}

impl Store {
    /// Locate (creating on first use) the store for the repository
    /// containing `cwd`. Precedence: `ARC_DATA_DIR` (exact directory for
    /// exactly one repository) > configured `data_root` (per-repo slug
    /// subdirectory, sandbox-friendly) > the repository's Git common dir.
    pub fn discover(cwd: &Path) -> Result<Store> {
        Self::discover_with(cwd, false)
    }

    /// Locate the store as `discover` does, for a command that records
    /// nothing. The invoking checkout's policy governs only appends, so it is
    /// never parsed here: a checkout whose policy is unreadable can still be
    /// reported on, and every append through this handle is refused.
    pub fn discover_for_reading(cwd: &Path) -> Result<Store> {
        Self::discover_with(cwd, true)
    }

    fn discover_with(cwd: &Path, read_only: bool) -> Result<Store> {
        let root = Self::resolve_root(cwd)?;
        // Read the invoking checkout's policy before creating anything. An
        // unreadable local policy fails with the filesystem untouched, and
        // repository-wide appends have no change target to read instead.
        let checkout = if read_only {
            None
        } else {
            gitio::toplevel(cwd).ok()
        };
        let (require_declared_actor, require_declared_actor_sources) = match checkout {
            Some(top) => {
                let policy = crate::policy::load(&top)?;
                let required = policy.policy.require_declared_actor;
                let sources = if required {
                    policy
                        .sources
                        .sources_for("policy.require_declared_actor=true")
                } else {
                    Vec::new()
                };
                (required, sources)
            }
            // A store opened outside a repository, or for reading, has no
            // policy to honour.
            None => (false, Vec::new()),
        };
        create_private_dir(&root)?;
        let config_path = root.join("config.json");
        let repository_id = match fs::read(&config_path) {
            Ok(bytes) => {
                let cfg: StoreConfig =
                    serde_json::from_slice(&bytes).context("malformed arc config.json")?;
                ensure_readable_format(cfg.schema_version, &config_path)?;
                cfg.repository_id
            }
            Err(_) => {
                let cfg = StoreConfig {
                    schema_version: 1,
                    repository_id: ids::new_identifier(),
                    created_at: chrono::Utc::now(),
                };
                write_exclusive(&config_path, &serde_json::to_vec_pretty(&cfg)?).or_else(|_| {
                    // Lost a creation race: the winner's config is authoritative.
                    fs::read(&config_path)
                        .map(|_| ())
                        .context("arc config.json unreadable after creation race")
                })?;
                let cfg: StoreConfig = serde_json::from_slice(&fs::read(&config_path)?)?;
                // The winner of a creation race may be a newer build, so the
                // config now on disk is not necessarily the one written above.
                ensure_readable_format(cfg.schema_version, &config_path)?;
                cfg.repository_id
            }
        };
        let store = Store {
            operator_policy_path: Some(crate::policy::operator_path(cwd)?),
            root,
            repository_id,
            require_declared_actor,
            require_declared_actor_sources,
            read_only,
        };
        store.repair_missing_format_three_stamp()?;
        Ok(store)
    }

    /// Open an existing store at an exact root directory, read-only. Returns
    /// `None` when the directory is not an arc store (no `config.json`), so
    /// callers scanning a `data_root` can skip non-store entries. Never
    /// creates anything.
    pub fn open_at(root: &Path) -> Result<Option<Store>> {
        match Self::repository_id_at(root)? {
            Some(repository_id) => Ok(Some(Store {
                operator_policy_path: Some(root.join("operator-policy.toml")),
                root: root.to_path_buf(),
                repository_id,
                require_declared_actor: false,
                require_declared_actor_sources: Vec::new(),
                read_only: false,
            })),
            None => Ok(None),
        }
    }

    pub fn open_at_for(root: &Path, cwd: &Path) -> Result<Option<Store>> {
        let mut store = Self::open_at(root)?;
        if let Some(store) = &mut store {
            store.operator_policy_path = Some(crate::policy::operator_path(cwd)?);
        }
        Ok(store)
    }

    pub fn resolve_root(cwd: &Path) -> Result<PathBuf> {
        if let Some(dir) = std::env::var_os("ARC_DATA_DIR") {
            return Ok(PathBuf::from(dir));
        }
        let config = crate::config::load()?;
        let common = gitio::common_dir(cwd)
            .context("not inside a Git repository (and no path override is set)")?;
        if let Some(data_root) = config.data_root {
            // Key by the main repository path (the common dir's parent),
            // never a worktree path: all worktrees share one ledger.
            let repo_path = if common.file_name().is_some_and(|n| n == ".git") {
                common.parent().unwrap_or(&common).to_path_buf()
            } else {
                common.clone()
            };
            return Ok(data_root.join(crate::config::path_slug(&repo_path)));
        }
        Ok(common.join("arc"))
    }

    fn changes_dir(&self) -> PathBuf {
        self.root.join("changes")
    }

    fn events_dir(&self, change_id: &str) -> PathBuf {
        self.changes_dir().join(change_id).join("events")
    }

    /// Events about the repository rather than about one change. A history
    /// rewrite is not change-scoped: it happens to every recorded revision at
    /// once, and filing it under one change would misstate what it is.
    fn repository_events_dir(&self) -> PathBuf {
        self.root.join("repository").join("events")
    }

    /// The reserved `change_id` a repository-scoped event carries, so the
    /// event schema stays one shape.
    pub const REPOSITORY_SCOPE: &'static str = "repository";

    fn append_repository_event_locked(&self, event: &Event) -> Result<()> {
        self.ensure_repository_event_allowed(event)?;
        self.stamp_session_resolution(event.session_resolution)?;
        self.stamp_format_for(&event.payload)?;
        let dir = self.repository_events_dir();
        create_private_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", event.event_id));
        let mut body = serde_json::to_vec_pretty(event)?;
        body.push(b'\n');
        write_exclusive(&path, &body)
            .with_context(|| format!("event {} already exists", event.event_id))
    }

    pub(crate) fn ensure_repository_event_allowed(&self, event: &Event) -> Result<()> {
        self.refuse_read_only()?;
        self.refuse_undeclared_author(event)?;
        if event.event_id.is_empty() {
            Ok(())
        } else {
            ids::validate_id_component(&event.event_id)
        }
    }

    /// Repository-scoped events in ID order. A repository that has never had
    /// one has no directory, which is not an error.
    pub fn load_repository_events(&self) -> Result<Vec<Event>> {
        typed_repository_events(self.raw_repository_events_unseen(&BTreeSet::new())?)
    }

    /// One exact staging transition is retained per change until its event is durable.
    pub(crate) fn provisional_intent_path(&self, change: &str) -> Result<PathBuf> {
        ids::validate_id_component(change)?;
        Ok(self.root.join("provisional").join(format!("{change}.json")))
    }
    pub(crate) fn provisional_intent(
        &self,
        change: &str,
    ) -> Result<Option<crate::commands::provisional::Intent>> {
        let path = self.provisional_intent_path(change)?;
        match fs::read(&path) {
            Ok(bytes) => {
                let intent: crate::commands::provisional::Intent =
                    serde_json::from_slice(&bytes).context("malformed provisional intent")?;
                if intent.event.change_id != change
                    || intent.event.repository_id != self.repository_id
                {
                    bail!("provisional intent identity conflicts with this store/change");
                }
                Ok(Some(intent))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    pub(crate) fn write_provisional_intent(
        &self,
        intent: &mut crate::commands::provisional::Intent,
    ) -> Result<()> {
        self.refuse_read_only()?;
        let _ledger = self.lock_event_ids()?;
        if !intent.event.event_id.is_empty() {
            bail!("provisional intent already has an issued identity");
        }
        intent.event.event_id = self.issue_event_id()?.to_string();
        self.stamp_format_for(&intent.event.payload)?;
        let path = self.provisional_intent_path(&intent.event.change_id)?;
        create_private_dir_all(path.parent().context("intent has no parent")?)?;
        write_exclusive(&path, &serde_json::to_vec(intent)?)
    }
    pub(crate) fn provisional_intents(&self) -> Result<Vec<crate::commands::provisional::Intent>> {
        let directory = self.root.join("provisional");
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("cannot read provisional intents"),
        };
        let mut changes = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(change) = name.to_str().and_then(|name| name.strip_suffix(".json")) {
                changes.push(change.to_string());
            }
        }
        changes.sort();
        changes
            .into_iter()
            .map(|change| {
                self.provisional_intent(&change)?
                    .with_context(|| format!("provisional intent for {change} disappeared"))
            })
            .collect()
    }
    pub(crate) fn publish_provisional_event(&self, event: &Event) -> Result<()> {
        self.refuse_read_only()?;
        let _ledger = self.lock_event_ids()?;
        if self.reservation_abandoned(event)? {
            bail!(
                "provisional reservation {} was abandoned by recovery",
                event.event_id
            );
        }
        if !self.event_is_published(event)? {
            self.advance_event_id(&event.event_id)?;
            self.write_event(event)?;
        }
        self.sync_published_event(&serde_json::to_value(event)?)
    }

    /// Confirm the exact event's backing publication, loose or imported, by
    /// syncing its file and directory entry to stable storage.
    pub(crate) fn sync_published_event(&self, expected: &serde_json::Value) -> Result<()> {
        let change_id = expected["change_id"]
            .as_str()
            .context("event has no change ID")?;
        let event_id = expected["event_id"].as_str().context("event has no ID")?;
        ids::validate_id_component(change_id)?;
        ids::validate_id_component(event_id)?;
        let (file, directory) = self
            .published_event_file(expected, &self.events_dir(change_id), false)?
            .with_context(|| format!("published event {event_id} has no backing publication"))?;
        file.sync_all()?;
        File::open(directory)?.sync_all()?;
        Ok(())
    }
    pub(crate) fn clear_provisional_intent(&self, change: &str) -> Result<()> {
        let path = self.provisional_intent_path(change)?;
        fs::remove_file(&path)?;
        File::open(path.parent().context("intent has no parent")?)?
            .sync_all()
            .map_err(Into::into)
    }

    /// Where a rewrite writes down what it is about to do, beside the events
    /// its map will become. One repository, one rewrite at a time.
    pub fn rewrite_intent_path(&self) -> PathBuf {
        self.root.join("repository").join("rewrite-intent.json")
    }

    /// The rewrite an interrupted run left to be finished, if there is one.
    pub fn rewrite_intent(&self) -> Result<Option<crate::rewrite::RewriteIntent>> {
        let path = self.rewrite_intent_path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()))
            }
        };
        // An unreadable intent is fatal rather than absent: it stands between
        // a half-applied rewrite and the run that would finish it, and
        // treating it as absent is exactly the second rewrite it exists to
        // prevent.
        serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("malformed rewrite intent {}", path.display()))
    }

    /// Write down a rewrite before any of it is applied. Fsynced and linked
    /// into place, because a rewrite that outlived its own intent file is the
    /// case the file exists to rule out.
    pub fn write_rewrite_intent(&self, intent: &crate::rewrite::RewriteIntent) -> Result<()> {
        let path = self.rewrite_intent_path();
        create_private_dir_all(
            path.parent()
                .context("the rewrite intent path has no parent directory")?,
        )?;
        let mut body = serde_json::to_vec_pretty(intent)?;
        body.push(b'\n');
        write_exclusive(&path, &body).with_context(|| {
            format!(
                "a rewrite is already in progress; finish it by running `arc rewrite sign` or \
                 `arc rewrite trailers` again, or remove {}",
                path.display()
            )
        })
    }

    /// Drop the intent, which is what says the rewrite finished.
    pub fn clear_rewrite_intent(&self) -> Result<()> {
        let path = self.rewrite_intent_path();
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("cannot remove {}", path.display())),
        }
    }

    /// Serialize state-derived transitions for one change across processes.
    pub fn lock_transition(&self, change_id: &str) -> Result<TransitionLock> {
        ids::validate_id_component(change_id)?;
        self.lock_file(
            &format!("{change_id}.lock"),
            "change transition",
            LOCK_TIMEOUT,
        )
    }

    /// Serialize dependency graph reads and writes across every change.
    pub fn lock_graph(&self) -> Result<TransitionLock> {
        self.lock_file("graph.lock", "repository graph", LOCK_TIMEOUT)
    }

    /// Prove the store's advisory-lock path is writable without mutating state.
    /// Serialize writes to the repository's own events. They are not
    /// change-scoped, so a per-change lock does not order two imports of
    /// different changes — and the map they build is judged as a whole.
    pub fn lock_repository_events(&self) -> Result<TransitionLock> {
        self.lock_file("repository-events.lock", "repository events", LOCK_TIMEOUT)
    }

    pub fn lock_probe(&self) -> Result<TransitionLock> {
        self.lock_file("probe.lock", "writability probe", LOCK_TIMEOUT)
    }

    fn lock_format(&self) -> Result<TransitionLock> {
        self.lock_file("format.lock", "store format", LOCK_TIMEOUT)
    }

    /// Take after transition/replica locks and before format/publication locks.
    /// Only filesystem work belongs inside this guard.
    pub(crate) fn lock_event_ids(&self) -> Result<TransitionLock> {
        self.refuse_read_only()?;
        self.lock_event_cursor()
    }

    /// A consistent read of reservations and visible events. Release this
    /// guard before running Git, hooks, or any consumer of the event stream.
    pub(crate) fn lock_event_cursor(&self) -> Result<TransitionLock> {
        self.lock_file("event-ids.lock", "ledger event IDs", LOCK_TIMEOUT)
    }

    fn read_id_marker(&self, name: &str) -> Result<Option<String>> {
        match fs::read_to_string(self.root.join(name)) {
            Ok(value) => {
                let id = value.trim();
                if !id.is_empty() {
                    ids::validate_id_component(id)?;
                }
                Ok(Some(id.to_string()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("cannot read {name} marker")),
        }
    }

    fn write_id_marker(&self, name: &str, id: &str) -> Result<()> {
        crate::commands::write_atomically(
            &self.root.join(name),
            id.as_bytes(),
            crate::commands::FileMode::Create(0o600),
        )
    }

    /// Recover the sequence from each event namespace once. Ordinary appends
    /// read only the two constant-size markers. The floor is written first,
    /// before publication, so interruption cannot hide an issued ID.
    fn event_id_floor(&self) -> Result<String> {
        let marker = self.read_id_marker("event-id")?;
        let witness = self.read_id_marker("event-id-floor")?;
        if let (Some(marker), Some(witness)) = (marker.as_ref(), witness.as_ref()) {
            if marker >= witness {
                return Ok(marker.clone());
            }
        }
        let mut newest = marker.into_iter().chain(witness).max().unwrap_or_default();
        let mut include = |id: &str| -> Result<()> {
            ids::validate_id_component(id)?;
            if id > newest.as_str() {
                newest = id.to_string();
            }
            Ok(())
        };
        for change in self.change_dir_ids(&self.changes_dir())? {
            for id in event_file_stems(&self.events_dir(&change))? {
                include(&id)?;
            }
        }
        for id in event_file_stems(&self.repository_events_dir())? {
            include(&id)?;
        }
        for batch in import_batches(&self.root)? {
            for event in batch.events.iter().chain(&batch.repository_events) {
                include(
                    event["event_id"]
                        .as_str()
                        .context("imported event has no ID")?,
                )?;
            }
        }
        for event in crate::replica::load_events(self)? {
            include(&event.event_id)?;
        }
        for intent in self.provisional_intents()? {
            include(&intent.event.event_id)?;
        }
        for id in event_file_stems(&self.root.join("provisional-abandoned"))? {
            include(&id)?;
        }
        self.write_id_marker("event-id-floor", &newest)?;
        self.write_id_marker("event-id", &newest)?;
        Ok(newest)
    }

    /// Advance before publication; imported identities are preserved and only
    /// contribute a floor for subsequent local issuance.
    /// The caller holds the ledger ID lock through the subsequent publication.
    pub(crate) fn advance_event_id(&self, id: &str) -> Result<()> {
        ids::validate_id_component(id)?;
        let newest = self.event_id_floor()?.max(id.to_string());
        self.write_id_marker("event-id-floor", &newest)?;
        self.write_id_marker("event-id", &newest)
    }

    fn reservation_abandoned(&self, event: &Event) -> Result<bool> {
        ids::validate_id_component(&event.event_id)?;
        let path = self
            .root
            .join("provisional-abandoned")
            .join(format!("{}.json", event.event_id));
        match fs::read(&path) {
            Ok(bytes) => {
                if serde_json::from_slice::<serde_json::Value>(&bytes)?
                    != serde_json::to_value(event)?
                {
                    bail!(
                        "abandoned reservation {} differs from its retained intent",
                        event.event_id
                    );
                }
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).context("cannot read abandoned event reservation"),
        }
    }

    /// The recovery caller has proved that this exact transition is unapplied
    /// and its change abandoned. Keep the intent as evidence; release its cursor.
    pub(crate) fn abandon_provisional_reservation(&self, event: &Event) -> Result<()> {
        let _ledger = self.lock_event_ids()?;
        let held = self
            .provisional_intent(&event.change_id)?
            .context("no retained provisional intent to abandon")?;
        if serde_json::to_value(&held.event)? != serde_json::to_value(event)? {
            bail!("provisional intent changed before abandoning its reservation");
        }
        if self.event_is_published(event)? {
            bail!("a published provisional event cannot be abandoned");
        }
        let directory = self.root.join("provisional-abandoned");
        create_private_dir_all(&directory)?;
        let path = directory.join(format!("{}.json", event.event_id));
        if !self.reservation_abandoned(event)? {
            write_exclusive(&path, &serde_json::to_vec(event)?)?;
        }
        File::open(&path)?.sync_all()?;
        File::open(directory)?.sync_all()?;
        Ok(())
    }

    /// Called under the ledger cursor lock. A reservation is its durable intent;
    /// exact publication or a durable recovery abandonment resolves it.
    pub(crate) fn oldest_event_reservation(&self) -> Result<Option<String>> {
        let mut oldest: Option<String> = None;
        for intent in self.provisional_intents()? {
            if !self.event_is_published(&intent.event)?
                && !self.reservation_abandoned(&intent.event)?
            {
                let id = intent.event.event_id;
                if oldest.as_ref().is_none_or(|held| id < *held) {
                    oldest = Some(id);
                }
            }
        }
        Ok(oldest)
    }

    /// The caller holds the ledger ID lock through publication or reservation.
    pub(crate) fn issue_event_id(&self) -> Result<ulid::Ulid> {
        let floor = self.event_id_floor()?;
        let unfloored = floor.is_empty();
        let floor = if floor.is_empty() {
            ulid::Ulid::default()
        } else {
            let parsed: ulid::Ulid = floor.parse().with_context(|| {
                format!("cannot issue a local event after non-ULID history {floor:?}")
            })?;
            if parsed.to_string() != floor {
                bail!("cannot issue a local event after noncanonical ULID history {floor:?}");
            }
            parsed
        };
        let issue = || -> Result<ulid::Ulid> {
            if unfloored {
                Ok(ids::new_event_id()?.parse()?)
            } else {
                ids::new_event_id_after(floor)
            }
        };
        #[cfg(test)]
        let id = match EVENT_ID_CANDIDATE.take() {
            Some(candidate) => {
                ids::after_floor(candidate, floor).context("event ID sequence exhausted")?
            }
            None => issue()?,
        };
        #[cfg(not(test))]
        let id = issue()?;
        self.write_id_marker("event-id-floor", &id.to_string())?;
        self.write_id_marker("event-id", &id.to_string())?;
        Ok(id)
    }

    /// Issue and publish under the same ledger-wide lock. No caller work,
    /// hook, or subprocess runs between these two operations.
    pub(crate) fn append_local_event(&self, event: &mut LocalEvent) -> Result<()> {
        let _ledger = self.lock_event_ids()?;
        if !event.event_id.is_empty() {
            bail!("local event already has an issued identity");
        }
        event.event_id = self.issue_event_id()?.to_string();
        self.write_event(event)
    }

    pub(crate) fn append_local_repository_event(&self, event: &mut LocalEvent) -> Result<()> {
        let _ledger = self.lock_event_ids()?;
        if !event.event_id.is_empty() {
            bail!("local event already has an issued identity");
        }
        event.event_id = self.issue_event_id()?.to_string();
        self.append_repository_event_locked(event)
    }

    /// Create and return the directory used for probe-only event-path writes.
    pub fn probe_events_dir(&self) -> Result<PathBuf> {
        let dir = self.changes_dir();
        create_private_dir_all(&dir)?;
        Ok(dir)
    }

    /// Serialize integrations that mutate the same target branch/worktree.
    pub fn lock_target(&self, target: &str) -> Result<TransitionLock> {
        if target.is_empty() {
            bail!("target branch cannot be empty");
        }
        let digest = Sha256::digest(target.as_bytes());
        self.lock_file(
            &format!("target-{}.lock", hex::encode(digest)),
            "integration target",
            TARGET_LOCK_TIMEOUT,
        )
    }

    fn lock_file(
        &self,
        name: &str,
        description: &str,
        timeout: Duration,
    ) -> Result<TransitionLock> {
        let dir = self.root.join("locks");
        create_private_dir_all(&dir)?;
        let path = dir.join(name);
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("cannot open {description} lock {}", path.display()))?;
        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(TransitionLock(file)),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        bail!(
                            "{description} lock {} is busy; retry the command",
                            path.display()
                        );
                    }
                    thread::sleep(LOCK_RETRY);
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(error)
                        .with_context(|| format!("cannot lock {description} {}", path.display()));
                }
            }
        }
    }

    /// Every change in the ledger. A ledger with no changes directory has no
    /// changes; one whose directory cannot be read is an error, never empty.
    pub fn list_change_ids(&self) -> Result<Vec<String>> {
        let dir = self.changes_dir();
        let mut ids_out = self.change_dir_ids(&dir)?;
        for batch in import_batches(&self.root)? {
            ids_out.insert(batch.change_id);
        }
        Ok(ids_out.into_iter().collect())
    }

    /// `list_change_ids` over import batches the caller already holds, so a
    /// reader that consults the batches for several purposes reads and
    /// validates them once.
    pub fn list_change_ids_in(&self, batches: &[ImportBatch]) -> Result<Vec<String>> {
        let mut ids_out = self.change_dir_ids(&self.changes_dir())?;
        ids_out.extend(batches.iter().map(|batch| batch.change_id.clone()));
        Ok(ids_out.into_iter().collect())
    }

    fn change_dir_ids(&self, dir: &Path) -> Result<BTreeSet<String>> {
        let mut ids_out = BTreeSet::new();
        if dir.is_dir() {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.file_type()?.is_dir() && ids::validate_id_component(&name).is_ok() {
                    ids_out.insert(name);
                }
            }
        }
        Ok(ids_out)
    }

    /// Resolve a user-supplied change reference: exact ID, else unique
    /// prefix (which covers bare slugs, since IDs are slug-prefixed).
    pub fn resolve_change(&self, needle: &str) -> Result<String> {
        ids::validate_id_component(needle)?;
        let all = self.list_change_ids()?;
        if all.iter().any(|c| c == needle) {
            return Ok(needle.to_string());
        }
        let matches: Vec<&String> = all.iter().filter(|c| c.starts_with(needle)).collect();
        match matches.len() {
            0 => bail!("no change matches {needle:?}"),
            1 => Ok(matches[0].clone()),
            _ => bail!(
                "ambiguous change {needle:?}: matches {}",
                matches
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// Resolve a comment or finding event ID, then a finding ID, exactly or by
    /// unique prefix.
    pub fn resolve_discussion_event(&self, change_id: &str, needle: &str) -> Result<Event> {
        ids::validate_id_component(needle)?;
        let events = self.load_events(change_id)?;
        let matches = events
            .iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    Payload::CommentAdded { .. }
                        | Payload::FindingAdded { .. }
                        | Payload::AuditFindingAdded { .. }
                )
            })
            .filter(|event| event.event_id == needle || event.event_id.starts_with(needle))
            .collect::<Vec<_>>();
        if let Some(event) = matches.iter().find(|event| event.event_id == needle) {
            return Ok((*event).clone());
        }
        match matches.len() {
            0 => {}
            1 => return Ok(matches[0].clone()),
            _ => bail!(
                "ambiguous discussion event {needle:?}: matches {}",
                matches
                    .iter()
                    .map(|event| event.event_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }

        let finding_matches = events
            .iter()
            .flat_map(|event| match &event.payload {
                Payload::FindingAdded { finding_id, .. }
                | Payload::AuditFindingAdded { finding_id, .. }
                    if finding_id == needle || finding_id.starts_with(needle) =>
                {
                    vec![(event, finding_id)]
                }
                Payload::VerdictRecorded { findings, .. } => findings
                    .iter()
                    .filter(|finding| {
                        finding.finding_id == needle || finding.finding_id.starts_with(needle)
                    })
                    .map(|finding| (event, &finding.finding_id))
                    .collect(),
                Payload::AuditVerdictRecorded { findings, .. } => findings
                    .iter()
                    .filter(|finding| {
                        finding.finding_id == needle || finding.finding_id.starts_with(needle)
                    })
                    .map(|finding| (event, &finding.finding_id))
                    .collect(),
                _ => Vec::new(),
            })
            .collect::<Vec<_>>();
        let resolved_finding = |event: &Event, finding_id: &str| {
            let mut resolved = event.clone();
            if let Payload::VerdictRecorded {
                patchset_id,
                findings,
                ..
            } = &event.payload
            {
                let finding = findings
                    .iter()
                    .find(|finding| finding.finding_id == finding_id)
                    .expect("matched inline finding remains present");
                resolved.payload = Payload::FindingAdded {
                    finding_id: finding.finding_id.clone(),
                    blocking: finding.blocking,
                    severity: finding.severity,
                    summary: finding.summary.clone(),
                    body: finding.body.clone(),
                    patchset_id: Some(patchset_id.clone()),
                    anchor: finding.anchor.clone(),
                };
            } else if let Payload::AuditVerdictRecorded { findings, .. } = &event.payload {
                let finding = findings
                    .iter()
                    .find(|finding| finding.finding_id == finding_id)
                    .expect("matched inline audit finding remains present");
                resolved.payload = Payload::AuditFindingAdded {
                    finding_id: finding.finding_id.clone(),
                    blocking: finding.blocking,
                    severity: finding.severity,
                    summary: finding.summary.clone(),
                    body: finding.body.clone(),
                    anchor: finding.anchor.clone(),
                };
            }
            resolved.event_id = finding_id.to_string();
            resolved
        };
        if let Some((event, finding_id)) = finding_matches
            .iter()
            .find(|(_, finding_id)| finding_id.as_str() == needle)
        {
            return Ok(resolved_finding(event, finding_id));
        }
        match finding_matches.len() {
            0 => bail!("no discussion event matches {needle:?}"),
            1 => {
                let (event, finding_id) = finding_matches[0];
                Ok(resolved_finding(event, finding_id))
            }
            _ => bail!(
                "ambiguous discussion event {needle:?}: matches {}",
                finding_matches
                    .iter()
                    .map(|(_, finding_id)| finding_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// Append one event. The file is created exclusively; a collision on
    /// a ULID event ID indicates a real bug and fails loudly.
    #[cfg(test)]
    pub fn append_event(&self, event: &Event) -> Result<()> {
        let _ledger = self.lock_event_ids()?;
        self.advance_event_id(&event.event_id)?;
        self.append_event_locked(event)
    }

    #[cfg(test)]
    fn append_event_locked(&self, event: &Event) -> Result<()> {
        self.refuse_read_only()?;
        self.refuse_undeclared_author(event)?;
        ids::validate_id_component(&event.change_id)?;
        ids::validate_id_component(&event.event_id)?;
        let key = (self.root.clone(), event.change_id.clone());
        let known = OPENING_IDS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .cloned();
        let opening_id = if let Some(id) = known {
            let path = self.events_dir(&event.change_id).join(format!("{id}.json"));
            match fs::read(&path) {
                Ok(bytes) => {
                    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
                    if value["event_id"] != id
                        || value["change_id"] != event.change_id
                        || value["event_type"] != "change-opened"
                    {
                        bail!("known opening event changed; refusing append");
                    }
                    Some(id)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => self
                    .raw_events(&event.change_id)?
                    .into_iter()
                    .find(|(_, value)| value["event_type"] == "change-opened")
                    .map(|(id, _)| id),
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("cannot inspect opening event {}", path.display())
                    })
                }
            }
        } else if self
            .list_change_ids()?
            .iter()
            .any(|id| id == &event.change_id)
        {
            self.raw_events(&event.change_id)?
                .into_iter()
                .find(|(_, value)| value["event_type"] == "change-opened")
                .map(|(id, _)| id)
        } else {
            None
        };
        if let Some(opening_id) = opening_id {
            if event.event_id < opening_id {
                bail!(
                    "event {} sorts before change {} opening {}; nothing was written",
                    event.event_id,
                    event.change_id,
                    opening_id
                );
            }
        }
        self.write_event(event)
    }

    fn write_event(&self, event: &Event) -> Result<()> {
        #[cfg(test)]
        if let Some((issued, release)) = LOCAL_PUBLICATION_PAUSE.take() {
            issued.send(()).unwrap();
            release.recv().unwrap();
        }
        self.refuse_read_only()?;
        self.refuse_undeclared_author(event)?;
        self.stamp_session_resolution(event.session_resolution)?;
        self.stamp_format_for(&event.payload)?;
        ids::validate_id_component(&event.change_id)?;
        ids::validate_id_component(&event.event_id)?;
        let dir = self.events_dir(&event.change_id);
        create_private_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", event.event_id));
        let mut body = serde_json::to_vec_pretty(event)?;
        body.push(b'\n');
        write_exclusive(&path, &body)
            .with_context(|| format!("event {} already exists", event.event_id))
    }

    /// Check the exact event after an append error, including a directory
    /// fsync failure after publication. An unreadable or different file is
    /// ambiguous and must not be treated as an absent event.
    pub fn event_is_published(&self, event: &Event) -> Result<bool> {
        ids::validate_id_component(&event.change_id)?;
        self.event_is_published_at(event, &self.events_dir(&event.change_id), false)
    }

    /// Where an event stands after its append reported an error: `visible-unconfirmed`
    /// when the exact event is readable but its publication did not complete,
    /// `absent` when it is not in the ledger, `unknown` when the ledger could
    /// not be read to tell.
    pub(crate) fn publication_after_failed_append(&self, event: &Event) -> &'static str {
        match self.event_is_published(event) {
            Ok(true) => "visible-unconfirmed",
            Ok(false) => "absent",
            Err(_) => "unknown",
        }
    }

    pub(crate) fn repository_event_is_published(&self, event: &Event) -> Result<bool> {
        self.event_is_published_at(event, &self.repository_events_dir(), true)
    }

    fn event_is_published_at(
        &self,
        event: &Event,
        directory: &Path,
        repository: bool,
    ) -> Result<bool> {
        if event.event_id.is_empty() {
            return Ok(false);
        }
        Ok(self
            .published_event_file(&serde_json::to_value(event)?, directory, repository)?
            .is_some())
    }

    /// Retain the descriptor whose complete content was checked, so durability
    /// confirmation flushes that publication rather than reopening its name.
    fn published_event_file(
        &self,
        expected: &serde_json::Value,
        directory: &Path,
        repository: bool,
    ) -> Result<Option<(File, PathBuf)>> {
        let id = expected["event_id"].as_str().context("event has no ID")?;
        ids::validate_id_component(id)?;
        let path = directory.join(format!("{id}.json"));
        let mut backing = None;
        match File::open(&path) {
            Ok(file) => {
                let published: serde_json::Value = serde_json::from_reader(&file)?;
                if &published != expected {
                    bail!("published event {id} differs from the attempted event");
                }
                backing = Some((file, directory.to_path_buf()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect {}", path.display()))
            }
        }
        for (path, batch) in import_batches_with_paths(&self.root)? {
            let values = if repository {
                &batch.repository_events
            } else if expected["change_id"].as_str() == Some(batch.change_id.as_str()) {
                &batch.events
            } else {
                continue;
            };
            let mut matching = values
                .iter()
                .filter(|value| value["event_id"].as_str() == Some(id));
            let Some(published) = matching.next() else {
                continue;
            };
            if published != expected || matching.any(|value| value != expected) {
                bail!("published event {id} differs from the attempted event");
            }
            if backing.is_none() {
                let file = File::open(&path)?;
                let held: ImportBatch = serde_json::from_reader(&file)?;
                if serde_json::to_value(&held)? != serde_json::to_value(&batch)? {
                    bail!("import backing event {id} changed during confirmation");
                }
                backing = Some((
                    file,
                    path.parent().context("import has no parent")?.to_path_buf(),
                ));
            }
        }
        Ok(backing)
    }

    /// Record that this store now holds events an older build would skip.
    ///
    /// The format barrier only works if the store says which format it is. A
    /// ledger created by an older arc keeps its stamp until something writes
    /// an event that older arc would not understand — and a skipped lifecycle
    /// event is how a closed change gets read as open and closed a second way.
    /// Stamping at that moment, rather than on every open, means a build that
    /// only read a ledger never locks its owner out of it.
    fn stamp_session_resolution(
        &self,
        resolution: Option<crate::model::SessionResolution>,
    ) -> Result<()> {
        if resolution == Some(crate::model::SessionResolution::Unresolved) {
            self.stamp_format(Some(5))?;
        }
        Ok(())
    }

    fn stamp_format_for(&self, payload: &Payload) -> Result<()> {
        let introduced_in = match payload {
            Payload::ChangeProvisionallyIntegrated { .. }
            | Payload::ProvisionalIntegrationResolved { .. } => Some(7),
            Payload::VerdictRecorded {
                authority_observed: Some(_),
                ..
            } => Some(7),
            Payload::ExternalVerdictRecorded { .. }
            | Payload::ReadyToSend { .. }
            | Payload::IntegrationAsserted {
                external_reference: Some(_),
                ..
            }
            | Payload::ChangeIntegrated {
                authorization:
                    Some(crate::model::AuthorizationBasis {
                        external_verdict: Some(_),
                        ..
                    }),
                ..
            } => Some(4),
            Payload::ChangeIntegrated {
                authorization: Some(authorization),
                ..
            } if authorization.verdict_event_id.is_none() => Some(3),
            // An older reader follows an approval through every rewrite, the
            // ones that changed content included.
            Payload::HistoryRewritten { .. } => Some(6),
            // An older reader skips the candidate ledger and would report a
            // registered candidate's pin as unregistered, and skips relations
            // and would explain a change with recorded reads as having none.
            payload if is_candidate_payload(payload) => Some(7),
            Payload::ContextRead { .. }
            | Payload::ContextDeclared { .. }
            | Payload::ContextCaptureReported { .. } => Some(7),
            Payload::HistoryRewriteWithdrawn { .. }
            | Payload::ChangeRolesAsserted { .. }
            | Payload::ChangeCheckoutBound { .. } => Some(7),
            // An older reader skips a post-integration disposition and reports
            // the finding with no disposition after it shipped.
            Payload::PostIntegrationDispositionRecorded { .. } => Some(7),
            // An older reader projects a record that a change needs no entry
            // as an entry with an empty category and body.
            Payload::ChangelogRecorded {
                no_entry_reason: Some(_),
                ..
            } => Some(7),
            Payload::ChangeIntegrated { .. } | Payload::IntegrationAsserted { .. } => Some(2),
            _ => None,
        };
        self.stamp_format(introduced_in)
    }

    /// Decide the format barrier from imported JSON. Candidate event kinds
    /// share their classification with typed records.
    #[cfg(test)]
    fn stamp_format_for_value(&self, value: Option<&serde_json::Value>) -> Result<()> {
        if value
            .and_then(|value| value.get("session_resolution"))
            .and_then(serde_json::Value::as_str)
            == Some("unresolved")
        {
            self.stamp_format(Some(5))?;
        }
        let introduced_in = match value
            .and_then(|value| value.get("event_type"))
            .and_then(serde_json::Value::as_str)
        {
            Some("external-verdict-recorded") | Some("ready-to-send") => Some(4),
            Some("history-rewritten") => Some(6),
            Some("verdict-recorded")
                if value
                    .and_then(|value| value.get("authority_observed"))
                    .is_some_and(|observed| !observed.is_null()) =>
            {
                Some(7)
            }
            Some(event_type) if is_candidate_event_type(event_type) => Some(7),
            Some("context-read") | Some("context-declared") | Some("context-capture-reported") => {
                Some(7)
            }
            Some("history-rewrite-withdrawn")
            | Some("change-roles-asserted")
            | Some("change-checkout-bound") => Some(7),
            Some("post-integration-disposition-recorded") => Some(7),
            Some("changelog-recorded")
                if value
                    .and_then(|value| value.get("no_entry_reason"))
                    .is_some_and(|reason| !reason.is_null()) =>
            {
                Some(7)
            }
            Some("integration-asserted")
                if value
                    .and_then(|value| value.get("external_reference"))
                    .and_then(serde_json::Value::as_str)
                    .is_some() =>
            {
                Some(4)
            }
            Some("change-integrated")
                if value
                    .and_then(|value| value.get("authorization"))
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(authorization_has_external_verdict) =>
            {
                Some(4)
            }
            Some("change-integrated")
                if value
                    .and_then(|value| value.get("authorization"))
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(authorization_has_no_verdict) =>
            {
                Some(3)
            }
            Some("change-provisionally-integrated" | "provisional-integration-resolved") => Some(7),
            Some("change-integrated") | Some("integration-asserted") => Some(2),
            _ => None,
        };
        self.stamp_format(introduced_in)
    }

    fn stamp_format(&self, introduced_in: Option<u32>) -> Result<()> {
        let Some(introduced_in) = introduced_in else {
            return Ok(());
        };
        // Different changes have different transition locks, but all of them
        // update this one monotonic barrier. Serialize the read-modify-rename
        // so a stale writer for an older format cannot land after a newer one.
        let _format = self.lock_format()?;
        self.stamp_format_locked(introduced_in)
    }

    fn stamp_format_locked(&self, introduced_in: u32) -> Result<()> {
        let config_path = self.root.join("config.json");
        let mut cfg: StoreConfig = match fs::read(&config_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("malformed arc config.json")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let cfg = StoreConfig {
                    schema_version: introduced_in,
                    repository_id: self.repository_id.clone(),
                    created_at: chrono::Utc::now(),
                };
                write_exclusive(&config_path, &serde_json::to_vec_pretty(&cfg)?)?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        if cfg.schema_version >= introduced_in {
            return Ok(());
        }
        cfg.schema_version = introduced_in;
        let body = serde_json::to_vec_pretty(&cfg)?;
        let temporary = config_path.with_extension(format!("json.{}", ids::new_identifier()));
        fs::write(&temporary, &body)?;
        fs::rename(&temporary, &config_path)
            .with_context(|| format!("cannot stamp store format in {}", config_path.display()))
    }

    /// Repair stores written during the format-3 omission window. Those
    /// ledgers already contain waiver-only integration events that a format-2
    /// reader cannot decode, so restoring the barrier records an existing fact
    /// rather than upgrading an otherwise old-readable store on mere access.
    fn repair_missing_format_three_stamp(&self) -> Result<()> {
        let config_path = self.root.join("config.json");
        let cfg: StoreConfig = serde_json::from_slice(&fs::read(&config_path)?)
            .context("malformed arc config.json")?;
        if cfg.schema_version >= 3 || !self.contains_waiver_only_integration()? {
            return Ok(());
        }
        self.stamp_format(Some(3))
    }

    fn contains_waiver_only_integration(&self) -> Result<bool> {
        let changes = self.changes_dir();
        let entries = match fs::read_dir(&changes) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", changes.display()))
            }
        };
        for change in entries {
            let events = change?.path().join("events");
            let event_entries = match fs::read_dir(&events) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error).with_context(|| format!("cannot read {}", events.display()))
                }
            };
            for event in event_entries {
                let path = event?.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let Ok(value) = serde_json::from_slice::<serde_json::Value>(&fs::read(&path)?)
                else {
                    continue;
                };
                if value.get("event_type").and_then(serde_json::Value::as_str)
                    == Some("change-integrated")
                    && value
                        .get("authorization")
                        .and_then(serde_json::Value::as_object)
                        .is_some_and(authorization_has_no_verdict)
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(crate) fn refuse_read_only(&self) -> Result<()> {
        if self.read_only {
            bail!("this ledger handle was opened for reading and records nothing");
        }
        Ok(())
    }

    /// Under `require_declared_actor`, refuse to record an event whose author
    /// nobody claimed.
    ///
    /// The check lives at the append rather than at the command, because no
    /// list of writing commands stays accurate as commands are added, and what
    /// the policy is about is the permanence of the record.
    ///
    /// A bundle import is deliberately outside this. Its events are another
    /// repository's history being transferred, not this session's claim about
    /// who acted, and a ledger that cannot receive history is worse than one
    /// that receives an identity it would not have written itself.
    fn refuse_undeclared_author(&self, event: &Event) -> Result<()> {
        let delegated = event
            .on_behalf_of
            .as_deref()
            .is_some_and(|subject| !subject.trim().is_empty());
        let declared =
            event.actor_source.is_some_and(ActorSource::declared) && !event.actor.trim().is_empty();
        if delegated || declared {
            return Ok(());
        }
        if !self.require_declared_actor {
            return Ok(());
        }
        bail!(
            "policy requires a declared actor (declared by {}): {:?} on event {} was not declared by anyone. Pass --actor or set ARC_ACTOR.",
            if self.require_declared_actor_sources.is_empty() {
                "source unavailable".to_string()
            } else {
                self.require_declared_actor_sources.join(", ")
            },
            event.actor,
            event.event_id
        )
    }

    /// The current view of one change: its events reduced, with every
    /// recorded revision followed forward through the repository's recorded
    /// history rewrites.
    ///
    /// Reducing the events directly answers in the revisions that were
    /// recorded, which after a rewrite name commits this repository does not
    /// hold. Both answers are legitimate and they are wanted in different
    /// places, so the store offers the resolved one and every reader that
    /// wants the repository's own terms takes it from here rather than
    /// remembering to resolve.
    ///
    /// A projection whose rewrites cannot be read is refused rather than
    /// answered without them. An unreadable map and no rewrite at all are
    /// different facts that look identical in the answer: every revision would
    /// come back at its pre-rewrite value, naming commits this repository does
    /// not hold, and every reader would present that as the current state.
    /// `arc doctor` reports the contradiction as `invalid-rewrite-mapping`,
    /// which is where the refusal points.
    pub fn project_state(
        &self,
        events: &[Event],
        rewrites: &crate::rewrite::RewriteMap,
    ) -> Result<crate::state::ChangeState> {
        self.project_state_in(&self.privilege_scope()?, events, rewrites)
    }

    /// `project_state` judged by a scope the caller derived for this read.
    pub fn project_state_in(
        &self,
        scope: &Arc<PrivilegeScope>,
        events: &[Event],
        rewrites: &crate::rewrite::RewriteMap,
    ) -> Result<crate::state::ChangeState> {
        let mut state = crate::state::reduce_following(events, rewrites)?;
        state.apply_privilege_scope(Arc::clone(scope), events);
        crate::replica::localize_change(&self.repository_id, events, &mut state);
        Ok(state)
    }

    pub fn state(&self, change_id: &str) -> Result<crate::state::ChangeState> {
        self.state_in(&self.privilege_scope()?, change_id)
    }

    /// `state` judged by a scope the caller derived for this read.
    pub fn state_in(
        &self,
        scope: &Arc<PrivilegeScope>,
        change_id: &str,
    ) -> Result<crate::state::ChangeState> {
        let events = self.load_events(change_id)?;
        self.project_state_in(scope, &events, &self.rewrites()?)
    }

    /// What the receiver permits, derived from the ledger as it stands.
    ///
    /// The scope is a value the caller holds for one read, never state kept on
    /// the store: an import, an append or a replica change made through any
    /// handle in the process changes what the next derivation answers, and a
    /// scope kept past that would judge events by permissions that no longer
    /// hold. A reader of several changes derives it once and passes it to
    /// `state_in` and `project_state_in`; a reader that writes between reads
    /// derives a fresh one for each.
    pub fn privilege_scope(&self) -> Result<Arc<PrivilegeScope>> {
        Ok(Arc::new(PrivilegeScope::for_store(self)?))
    }

    /// Open one read of the ledger as it stands.
    pub fn read(&self) -> Result<LedgerRead<'_>> {
        Ok(LedgerRead {
            store: self,
            batches: import_batches(&self.root)?,
            scope: Default::default(),
            rewrites: Default::default(),
        })
    }

    /// Readable changes for an aggregate view. A per-change failure is named
    /// on stderr; failures reading the shared ledger or rewrite map refuse
    /// the observation because they cannot be attributed to one change.
    ///
    /// The scope is derived once for the whole view, so the cost of an
    /// aggregate read does not multiply by the number of changes.
    pub fn readable_states(&self) -> Result<BTreeMap<String, crate::state::ChangeState>> {
        let read = self.read()?;
        read.rewrites()?;
        let mut states = BTreeMap::new();
        for change_id in read.change_ids()? {
            let result = read.events(&change_id).and_then(|events| {
                let mut state = crate::state::reduce_following(&events, read.rewrites()?)?;
                state.apply_privilege_scope(Arc::clone(read.scope()?), &events);
                crate::replica::localize_change(&self.repository_id, &events, &mut state);
                Ok(state)
            });
            match result {
                Ok(state) => {
                    states.insert(change_id, state);
                }
                Err(error) => eprintln!("warning: unreadable change {change_id}: {error:#}"),
            }
        }
        Ok(states)
    }

    /// Every history rewrite this repository recorded, flattened into one
    /// old-to-fate mapping. For readers that hold events already and still
    /// need the repository's own terms.
    pub fn rewrites(&self) -> Result<crate::rewrite::RewriteMap> {
        crate::rewrite::RewriteMap::load(self)
            .context("cannot read the recorded history rewrites; `arc doctor` reports which")
    }

    /// `rewrites` over import batches the caller already holds.
    fn rewrites_in(&self, batches: &[ImportBatch]) -> Result<crate::rewrite::RewriteMap> {
        typed_repository_events(self.raw_repository_events_in(batches)?)
            .and_then(|events| crate::rewrite::RewriteMap::from_events(events.iter()))
            .context("cannot read the recorded history rewrites; `arc doctor` reports which")
    }

    /// All events of one change in ID order, preserving imported identities.
    pub fn load_events(&self, change_id: &str) -> Result<Vec<Event>> {
        typed_events(self.raw_events(change_id)?)
    }

    /// `load_events` over import batches the caller already holds.
    pub fn load_events_in(&self, change_id: &str, batches: &[ImportBatch]) -> Result<Vec<Event>> {
        typed_events(self.raw_events_in(change_id, batches)?)
    }

    /// Raw JSON events of one change. Export deliberately bypasses the
    /// typed Event enum so future event types and fields survive intact.
    pub fn raw_events(&self, change_id: &str) -> Result<Vec<(String, serde_json::Value)>> {
        self.raw_events_unseen(change_id, &BTreeSet::new())
    }

    /// `raw_events` over import batches the caller already holds.
    pub fn raw_events_in(
        &self,
        change_id: &str,
        batches: &[ImportBatch],
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let (events, directory_present) = self.event_files_unseen(change_id, &BTreeSet::new())?;
        self.finish_raw_events(
            change_id,
            events,
            directory_present,
            batches,
            &BTreeSet::new(),
        )
    }

    /// Read only raw events whose IDs the caller has not already observed.
    /// Follow mode uses this to rescan directory entries without reparsing the
    /// complete ledger on every poll.
    pub fn raw_events_unseen(
        &self,
        change_id: &str,
        seen: &BTreeSet<String>,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let (events, directory_present) = self.event_files_unseen(change_id, seen)?;
        self.finish_raw_events(
            change_id,
            events,
            directory_present,
            &import_batches(&self.root)?,
            seen,
        )
    }

    /// The change's event files not in `seen`, and whether its directory
    /// exists.
    fn event_files_unseen(
        &self,
        change_id: &str,
        seen: &BTreeSet<String>,
    ) -> Result<(Vec<(String, serde_json::Value)>, bool)> {
        ids::validate_id_component(change_id)?;
        let dir = self.events_dir(change_id);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", dir.display()))
            }
        };
        let mut events = Vec::new();
        for entry in entries.into_iter().flatten() {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(event_id) = name.strip_suffix(".json") else {
                continue;
            };
            ids::validate_id_component(event_id)?;
            if seen.contains(event_id) {
                continue;
            }
            let path = entry.path();
            let value = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("malformed event file {}", path.display()))?;
            events.push((event_id.to_string(), value));
        }
        Ok((events, dir.is_dir()))
    }

    /// Merge the batches' events into one change's file events, then check and
    /// order the result.
    fn finish_raw_events(
        &self,
        change_id: &str,
        mut events: Vec<(String, serde_json::Value)>,
        directory_present: bool,
        batches: &[ImportBatch],
        seen: &BTreeSet<String>,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let mut found = directory_present;
        for batch in batches {
            if batch.change_id == change_id {
                found = true;
                merge_raw(&mut events, batch.events.iter().cloned(), seen)?;
            }
        }
        if !found {
            bail!("unknown change {change_id:?}");
        }
        for (id, value) in &events {
            if value["change_id"].as_str() != Some(change_id) {
                bail!("event {id} belongs to another change");
            }
        }
        events.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some((id, _)) = events
            .iter()
            .find(|(_, value)| value["event_type"] == "change-opened")
        {
            OPENING_IDS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert((self.root.clone(), change_id.to_string()), id.clone());
        }
        Ok(events)
    }

    /// Unseen raw JSON events from every change, globally ordered by event ID.
    /// This avoids decoding the typed Event enum so readers can forward unknown
    /// imported fields and event types.
    pub fn raw_events_all_unseen(
        &self,
        seen: &BTreeSet<String>,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let mut events = Vec::new();
        for change_id in self.list_change_ids()? {
            events.extend(self.raw_events_unseen(&change_id, seen)?);
        }
        // Repository-scoped events are part of this repository's history, and
        // an unscoped stream that omitted them would make them write-only.
        events.extend(self.raw_repository_events_unseen(seen)?);
        events.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(events)
    }

    /// Every repository event file, readable or not, as (id, bytes). Doctor
    /// walks this rather than the parsed listing: one unreadable file must not
    /// hide the state of every other, which is the whole point of the report.
    pub fn repository_event_files(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let dir = self.repository_events_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", dir.display()))
            }
        };
        let mut files = Vec::new();
        for entry in entries.into_iter().flatten() {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let Some(event_id) = name.strip_suffix(".json") else {
                continue;
            };
            files.push((event_id.to_string(), fs::read(dir.join(&name))?));
        }
        for batch in import_batches(&self.root)? {
            for value in batch.repository_events {
                let id = value["event_id"]
                    .as_str()
                    .context("imported repository event has no ID")?;
                if !files.iter().any(|(existing, _)| existing == id) {
                    files.push((id.to_string(), serde_json::to_vec(&value)?));
                }
            }
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(files)
    }

    /// Repository-scoped raw events not already seen, in ID order.
    pub fn raw_repository_events_unseen(
        &self,
        seen: &BTreeSet<String>,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let mut events = self.repository_event_files_unseen(seen)?;
        for batch in import_batches(&self.root)? {
            merge_raw(&mut events, batch.repository_events, seen)?;
        }
        events.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(events)
    }

    /// `raw_repository_events_unseen` over import batches the caller already
    /// holds.
    pub fn raw_repository_events_in(
        &self,
        batches: &[ImportBatch],
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let seen = BTreeSet::new();
        let mut events = self.repository_event_files_unseen(&seen)?;
        for batch in batches {
            merge_raw(&mut events, batch.repository_events.iter().cloned(), &seen)?;
        }
        events.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(events)
    }

    /// The IDs of the event files one change holds, named by file without
    /// reading any of them. A file `raw_events` would reject is listed too, so
    /// the answer is a superset of the IDs a read returns.
    pub fn event_file_ids(&self, change_id: &str) -> Result<BTreeSet<String>> {
        ids::validate_id_component(change_id)?;
        event_file_stems(&self.events_dir(change_id))
    }

    /// `event_file_ids` for the repository-scoped events.
    pub fn repository_event_file_ids(&self) -> Result<BTreeSet<String>> {
        event_file_stems(&self.repository_events_dir())
    }

    fn repository_event_files_unseen(
        &self,
        seen: &BTreeSet<String>,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let dir = self.repository_events_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", dir.display()))
            }
        };
        let mut events = Vec::new();
        for entry in entries.into_iter().flatten() {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let Some(event_id) = name.strip_suffix(".json") else {
                continue;
            };
            if seen.contains(event_id) {
                continue;
            }
            let value: serde_json::Value = serde_json::from_slice(&fs::read(dir.join(&name))?)
                .with_context(|| format!("malformed event file {}", dir.join(&name).display()))?;
            events.push((event_id.to_string(), value));
        }
        Ok(events)
    }

    /// Read an event without creating the store. Import uses this during
    /// its validate-and-plan phase so conflicts and dry-runs write nothing.
    pub fn raw_event_at(root: &Path, change_id: &str, event_id: &str) -> Result<Option<Vec<u8>>> {
        ids::validate_id_component(change_id)?;
        ids::validate_id_component(event_id)?;
        let path = root
            .join("changes")
            .join(change_id)
            .join("events")
            .join(format!("{event_id}.json"));
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                for batch in import_batches(root)? {
                    if batch.change_id == change_id {
                        if let Some(value) = batch
                            .events
                            .iter()
                            .find(|value| value["event_id"].as_str() == Some(event_id))
                        {
                            return Ok(Some(serde_json::to_vec(value)?));
                        }
                    }
                }
                Ok(None)
            }
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Return the local repository ID when the store already exists,
    /// without creating it for a dry-run.
    pub fn repository_id_at(root: &Path) -> Result<Option<String>> {
        let path = root.join("config.json");
        match fs::read(&path) {
            Ok(bytes) => {
                let cfg: StoreConfig = serde_json::from_slice(&bytes)
                    .with_context(|| format!("malformed {}", path.display()))?;
                ensure_readable_format(cfg.schema_version, &path)?;
                Ok(Some(cfg.repository_id))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Whether a repository event can be written: absent, or present with
    /// byte-identical content. Checked for every event before the first is
    /// written, so a contradiction cannot leave a partial import behind.
    pub fn repository_event_conflicts(&self, event_id: &str, bytes: &[u8]) -> Result<bool> {
        ids::validate_id_component(event_id)?;
        let incoming: serde_json::Value = serde_json::from_slice(bytes)?;
        Ok(self
            .raw_repository_events_unseen(&BTreeSet::new())?
            .iter()
            .any(|(id, existing)| id == event_id && !same_repository_event(existing, &incoming)))
    }

    pub fn publish_import(&self, batch: &ImportBatch) -> Result<()> {
        self.refuse_read_only()?;
        let _ledger = self.lock_event_ids()?;
        #[cfg(debug_assertions)]
        crate::import_authorization::test_rendezvous("format")?;
        let _format = self.lock_format()?;
        let policy_path = self
            .operator_policy_path
            .clone()
            .unwrap_or_else(|| self.root.join("operator-policy.toml"));
        let _publication = crate::policy::lock_publication(&policy_path)?;
        let current = crate::policy::replica_at(&policy_path)?;
        let values: Vec<_> = batch
            .events
            .iter()
            .cloned()
            .map(|value| ("change".into(), value))
            .chain(
                batch
                    .repository_events
                    .iter()
                    .cloned()
                    .map(|value| ("repository".into(), value)),
            )
            .collect();
        if let Some(receipt) = &batch.authorization {
            if crate::import_authorization::policy_digest(&current)? != receipt.policy_sha256 {
                return Err(crate::import_authorization::Refusal(
                    "authorization configuration moved before publication: operator replica policy"
                        .into(),
                )
                .into());
            }
            crate::import_authorization::validate_receipt_events(receipt, &values)?;
        } else if !current.publishers.is_empty()
            || crate::replica::status(self)?.project_id.is_some()
        {
            return Err(crate::import_authorization::Refusal("authorization configuration moved before publication: publishing enrollment required".into()).into());
        }
        crate::import_authorization::validate_import_identities(self, &values)?;
        if let Some(newest) = values
            .iter()
            .map(|(_, event)| {
                event["event_id"]
                    .as_str()
                    .context("imported event has no ID")
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .max()
        {
            self.advance_event_id(newest)?;
        }
        let dir = self.root.join("imports");
        create_private_dir_all(&dir).map_err(|error| crate::model::OperationFailure {
            published: false,
            message: format!("batch directory preparation failed: {error:#}"),
        })?;
        let path = dir.join(format!("{}.json", ids::new_identifier()));
        crate::replica::write_exclusive(&path, &serde_json::to_vec_pretty(batch)?).context(
            "import admission publication failed; inspect the published batch before retrying",
        )?;
        self.stamp_format_locked(7)
            .map_err(|error| crate::model::OperationFailure {
                published: true,
                message: format!("store-format maintenance failed: {error:#}"),
            })?;
        Ok(())
    }

    /// Write a repository-scoped event verbatim, as import does for a change.
    /// Idempotent: a rewrite already recorded here is the same fact.
    #[cfg(test)]
    pub fn append_raw_repository_event(&self, event_id: &str, bytes: &[u8]) -> Result<bool> {
        self.refuse_read_only()?;
        let _ledger = self.lock_event_ids()?;
        ids::validate_id_component(event_id)?;
        let value = serde_json::from_slice::<serde_json::Value>(bytes).ok();
        self.stamp_format_for_value(value.as_ref())?;
        let dir = self.repository_events_dir();
        create_private_dir_all(&dir)?;
        let path = dir.join(format!("{event_id}.json"));
        if path.exists() {
            // Same ID, different bytes is a contradiction, not a duplicate:
            // accepting it would keep the local mapping while reporting the
            // bundle imported, and every later resolution would use history
            // the bundle disagrees with.
            let existing: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            let incoming: serde_json::Value = serde_json::from_slice(bytes)?;
            if !same_repository_event(&existing, &incoming) {
                bail!(
                    "repository event {event_id} already exists here with different content; \
                     nothing was imported"
                );
            }
            return Ok(false);
        }
        self.advance_event_id(event_id)?;
        write_exclusive(&path, bytes)
            .with_context(|| format!("repository event {event_id} already exists during import"))?;
        Ok(true)
    }

    #[cfg(test)]
    pub fn append_raw_event(&self, change_id: &str, event_id: &str, bytes: &[u8]) -> Result<()> {
        self.refuse_read_only()?;
        let _ledger = self.lock_event_ids()?;
        ids::validate_id_component(change_id)?;
        ids::validate_id_component(event_id)?;
        // Import writes bytes rather than a typed payload, but the store it
        // writes into is the same one an older build may reopen. A bundle
        // carrying an integration event must stamp the format exactly as a
        // locally recorded one does.
        let value = serde_json::from_slice::<serde_json::Value>(bytes).ok();
        self.stamp_format_for_value(value.as_ref())?;
        let dir = self.events_dir(change_id);
        create_private_dir_all(&dir)?;
        let path = dir.join(format!("{event_id}.json"));
        self.advance_event_id(event_id)?;
        write_exclusive(&path, bytes)
            .with_context(|| format!("event {event_id} already exists during import"))
    }
}

/// The field of a `history-rewritten` event that records what the recording
/// repository found when it compared the commits.
pub const SIGNATURE_ONLY_FIELD: &str = "signature_only";

/// Whether two copies of one repository event are the same fact.
///
/// The signature-only judgement is the receiver's own: an import replaces the
/// sender's with one made against the objects it holds, so the copy it holds
/// and the copy a bundle carries are one event when they agree on everything
/// else.
fn same_repository_event(existing: &serde_json::Value, incoming: &serde_json::Value) -> bool {
    let bare = |value: &serde_json::Value| {
        let mut value = value.clone();
        if let Some(object) = value.as_object_mut() {
            object.remove(SIGNATURE_ONLY_FIELD);
        }
        value
    };
    bare(existing) == bare(incoming)
}

#[cfg(test)]
fn authorization_has_external_verdict(
    authorization: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    authorization
        .get("external_verdict")
        .is_some_and(|external| !external.is_null())
}

fn authorization_has_no_verdict(
    authorization: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    authorization
        .get("verdict_event_id")
        .is_none_or(serde_json::Value::is_null)
}

/// Whether this build may read a store at all.
///
/// A store written by a newer arc may hold event types this build would skip
/// as unknown — and skipping a lifecycle event means reading a closed change
/// as open, then closing it a second way. Refusing is the only honest answer,
/// and it has to hold on every path that opens a store, not just the one that
/// creates it.
fn ensure_readable_format(schema_version: u32, path: &Path) -> Result<()> {
    if schema_version > crate::model::SCHEMA_VERSION {
        bail!(
            "{} was written by a newer arc (store format {schema_version}, this build \
             understands {}); upgrade arc rather than reading it with this build",
            path.display(),
            crate::model::SCHEMA_VERSION
        );
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    create_private_dir_all(path)
}

fn create_private_dir_all(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("cannot create {}", path.display()))
}

#[cfg(test)]
thread_local! {
    /// Makes the directory sync that completes `write_exclusive` fail on this
    /// thread, after the file is already visible.
    static FAIL_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Durably publishes a new file by fsyncing its contents, hard-linking it into
/// place, and fsyncing the containing directory.
fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<()> {
    let file_name = path
        .file_name()
        .context("exclusive-write path has no file name")?
        .to_string_lossy();
    let temporary =
        path.with_file_name(format!(".{file_name}.{}.tmp", crate::ids::new_identifier()));

    let publish = (|| -> Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("cannot create {}", temporary.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);

        // A same-directory hard link atomically publishes the fully written
        // inode while preserving create-new collision behavior. Temporary
        // names do not end in .json, so readers ignore a file left by a crash.
        fs::hard_link(&temporary, path)
            .with_context(|| format!("cannot create {}", path.display()))?;
        let dir = path
            .parent()
            .context("exclusive-write path has no parent directory")?;
        let directory =
            File::open(dir).with_context(|| format!("cannot fsync {}", dir.display()))?;
        #[cfg(test)]
        if FAIL_DIRECTORY_SYNC.get() {
            bail!("injected failure syncing {}", dir.display());
        }
        directory
            .sync_all()
            .with_context(|| format!("cannot fsync {}", dir.display()))?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    publish
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn test_store(root: &Path) -> Store {
        Store {
            operator_policy_path: None,
            root: root.to_path_buf(),
            repository_id: "repo".into(),
            require_declared_actor: false,
            require_declared_actor_sources: Vec::new(),
            read_only: false,
        }
    }

    fn ordering_event(change: &str, id: ulid::Ulid) -> Event {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1, "event_id": id.to_string(), "repository_id": "repo",
            "change_id": change, "actor": "tester", "created_at": chrono::Utc::now(),
            "event_type": "hold-set", "reason": "ordering"
        }))
        .unwrap()
    }

    fn ordering_draft(change: &str) -> LocalEvent {
        let mut event = ordering_event(change, ulid::Ulid::default());
        event.event_id.clear();
        LocalEvent::draft(event)
    }

    #[test]
    fn local_writers_issue_ids_in_publication_order_in_one_millisecond() {
        let dir = tempfile::tempdir().unwrap();
        let floor = ulid::Ulid::from_parts(ulid::Ulid::new().timestamp_ms() + 60_000, 0);
        test_store(dir.path())
            .append_event(&ordering_event("seed", floor))
            .unwrap();
        let (issued, wait) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (started, starting) = mpsc::channel();
        let (published, publication) = mpsc::channel();
        let root = dir.path().to_path_buf();
        let first = std::thread::spawn(move || {
            let store = test_store(&root);
            let mut event = ordering_draft("first");
            EVENT_ID_CANDIDATE.set(Some(floor.increment().unwrap().increment().unwrap()));
            LOCAL_PUBLICATION_PAUSE.set(Some((issued, released)));
            store.append_local_event(&mut event).unwrap();
            event.event_id.clone()
        });
        let root = dir.path().to_path_buf();
        let second = std::thread::spawn(move || {
            wait.recv().unwrap();
            let store = test_store(&root);
            let mut event = ordering_draft("second");
            EVENT_ID_CANDIDATE.set(Some(floor.increment().unwrap()));
            started.send(()).unwrap();
            store.append_local_event(&mut event).unwrap();
            published.send(event.event_id.clone()).unwrap();
            assert_eq!(
                event.event_id.parse::<ulid::Ulid>().unwrap().timestamp_ms(),
                floor.timestamp_ms()
            );
        });
        starting.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            publication.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release.send(()).unwrap();
        let first = first.join().unwrap();
        let second_id = publication.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(first < second_id);
        second.join().unwrap();
    }

    #[test]
    fn local_id_marker_recovers_after_deletion_and_when_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path());
        let floor = ulid::Ulid::from_parts(ulid::Ulid::new().timestamp_ms() + 60_000, 0);
        let event = ordering_event("first", floor);
        store.append_event(&event).unwrap();
        let marker = dir.path().join("event-id");
        assert_eq!(fs::read_to_string(&marker).unwrap().trim(), event.event_id);
        fs::remove_file(&marker).unwrap();
        fs::remove_file(dir.path().join("event-id-floor")).unwrap();
        let mut next = ordering_draft("second");
        EVENT_ID_CANDIDATE.set(Some(floor));
        store.append_local_event(&mut next).unwrap();
        assert_eq!(fs::read_to_string(&marker).unwrap().trim(), next.event_id);
        fs::write(&marker, floor.to_string()).unwrap();
        let mut last = ordering_draft("third");
        EVENT_ID_CANDIDATE.set(Some(floor));
        store.append_local_event(&mut last).unwrap();
        assert!(last.event_id > next.event_id);
        assert_eq!(fs::read_to_string(&marker).unwrap().trim(), last.event_id);
    }

    #[test]
    fn an_initialized_local_append_does_not_read_other_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path());
        store
            .append_event(&ordering_event("seed", ulid::Ulid::new()))
            .unwrap();
        create_private_dir_all(&dir.path().join("changes/unreadable")).unwrap();
        fs::write(
            dir.path().join("changes/unreadable/events"),
            b"not a directory",
        )
        .unwrap();
        let mut event = ordering_draft("local");
        store.append_local_event(&mut event).unwrap();
        assert!(store.event_is_published(&event).unwrap());
        fs::remove_file(dir.path().join("event-id")).unwrap();
        assert!(store
            .append_local_event(&mut ordering_draft("recovery"))
            .is_err());
    }

    #[test]
    fn provisional_reservation_allows_later_local_publication_before_exact_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path());
        let mut intent = crate::commands::provisional::Intent {
            event: ordering_draft("reserved").into_event(),
            old_staging: None,
        };
        store.write_provisional_intent(&mut intent).unwrap();
        let reserved = intent.event.event_id.clone();
        // No ledger lock spans the work that binds refs to this identity.
        drop(store.lock_event_ids().unwrap());
        let other = test_store(dir.path());
        let mut later = ordering_draft("later");
        other.append_local_event(&mut later).unwrap();
        assert!(later.event_id > reserved);
        assert_eq!(
            other.oldest_event_reservation().unwrap().as_deref(),
            Some(reserved.as_str())
        );
        fs::remove_file(dir.path().join("event-id")).unwrap();
        store.publish_provisional_event(&intent.event).unwrap();
        assert_eq!(intent.event.event_id, reserved);
        assert!(later.event_id > reserved);
        assert_eq!(other.oldest_event_reservation().unwrap(), None);
        store.clear_provisional_intent("reserved").unwrap();
    }

    #[test]
    fn reservation_cursor_waits_for_the_oldest_and_abandonment_is_durable() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path());
        let mut first = crate::commands::provisional::Intent {
            event: ordering_draft("first").into_event(),
            old_staging: None,
        };
        let mut second = crate::commands::provisional::Intent {
            event: ordering_draft("second").into_event(),
            old_staging: None,
        };
        store.write_provisional_intent(&mut first).unwrap();
        store.write_provisional_intent(&mut second).unwrap();
        store.publish_provisional_event(&second.event).unwrap();
        assert_eq!(
            store.oldest_event_reservation().unwrap(),
            Some(first.event.event_id.clone())
        );
        store.abandon_provisional_reservation(&first.event).unwrap();
        assert_eq!(
            test_store(dir.path()).oldest_event_reservation().unwrap(),
            None
        );
        assert!(store.publish_provisional_event(&first.event).is_err());
        assert!(store.provisional_intent("first").unwrap().is_some());
        store.clear_provisional_intent("first").unwrap();
        store.clear_provisional_intent("second").unwrap();
        fs::remove_file(dir.path().join("event-id")).unwrap();
        fs::remove_file(dir.path().join("event-id-floor")).unwrap();
        let mut later = ordering_draft("later");
        store.append_local_event(&mut later).unwrap();
        assert!(later.event_id > second.event.event_id);
    }

    #[test]
    fn local_publication_refuses_exhausted_and_noncanonical_floors() {
        for floor in ["7ZZZZZZZZZZZZZZZZZZZZZZZZZ", "01arz3ndektsv4rrffq69g5fav"] {
            let dir = tempfile::tempdir().unwrap();
            let store = test_store(dir.path());
            let mut held = ordering_event("held", ulid::Ulid::default());
            held.event_id = floor.into();
            store.append_event(&held).unwrap();
            let mut local = ordering_draft("local");
            assert!(store.append_local_event(&mut local).is_err());
            assert!(local.event_id.is_empty());
            assert!(!store.events_dir("local").exists());
            assert_eq!(
                fs::read_to_string(dir.path().join("event-id")).unwrap(),
                floor
            );
        }
    }

    #[test]
    fn an_event_whose_directory_sync_failed_is_visible_but_unconfirmed() {
        let dir = tempfile::tempdir().unwrap();
        let config = StoreConfig {
            schema_version: 1,
            repository_id: "repo".into(),
            created_at: chrono::Utc::now(),
        };
        fs::write(
            dir.path().join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let store = test_store(dir.path());
        let event: Event = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "event_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "repository_id": "repo",
            "change_id": "change",
            "actor": "tester",
            "created_at": chrono::Utc::now(),
            "event_type": "candidate-retired",
            "candidate_id": "candidate",
        }))
        .unwrap();
        assert_eq!(store.publication_after_failed_append(&event), "absent");
        FAIL_DIRECTORY_SYNC.set(true);
        let failed = store.append_event(&event);
        FAIL_DIRECTORY_SYNC.set(false);
        assert!(failed.is_err());
        assert_eq!(
            store.publication_after_failed_append(&event),
            "visible-unconfirmed"
        );
        store
            .sync_published_event(&serde_json::to_value(&event).unwrap())
            .unwrap();
        let mut different = event.clone();
        different.actor = "other".into();
        assert_eq!(store.publication_after_failed_append(&different), "unknown");
    }

    #[test]
    fn imported_publications_are_visible_and_confirmable_in_both_scopes() {
        for repository in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let store = test_store(dir.path());
            let event: Event = serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "event_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "repository_id": "repo", "change_id": "change",
                "actor": "tester", "created_at": chrono::Utc::now(),
                "event_type": "candidate-retired", "candidate_id": "candidate",
            }))
            .unwrap();
            let value = serde_json::to_value(&event).unwrap();
            let batch = ImportBatch {
                schema: "arc-import-batch/1".into(),
                change_id: "change".into(),
                events: if repository {
                    Vec::new()
                } else {
                    vec![value.clone()]
                },
                repository_events: if repository { vec![value] } else { Vec::new() },
                authorization: None,
            };
            let imports = dir.path().join("imports");
            fs::create_dir(&imports).unwrap();
            fs::write(
                imports.join("batch.json"),
                serde_json::to_vec(&batch).unwrap(),
            )
            .unwrap();
            let visible = |event: &Event| {
                if repository {
                    store.repository_event_is_published(event)
                } else {
                    store.event_is_published(event)
                }
            };
            assert!(visible(&event).unwrap());
            if !repository {
                store
                    .sync_published_event(&serde_json::to_value(&event).unwrap())
                    .unwrap();
                store.publish_provisional_event(&event).unwrap();
                assert!(!store.events_dir(&event.change_id).exists());
            }
            let mut different = event.clone();
            different.actor = "other".into();
            assert!(visible(&different).is_err());
            let conflicting = ImportBatch {
                schema: "arc-import-batch/1".into(),
                change_id: "change".into(),
                events: if repository {
                    Vec::new()
                } else {
                    vec![serde_json::to_value(&different).unwrap()]
                },
                repository_events: if repository {
                    vec![serde_json::to_value(&different).unwrap()]
                } else {
                    Vec::new()
                },
                authorization: None,
            };
            let conflict = imports.join("conflict.json");
            fs::write(&conflict, serde_json::to_vec(&conflicting).unwrap()).unwrap();
            assert!(
                visible(&event).is_err(),
                "conflicting backing publications were accepted"
            );
            fs::remove_file(conflict).unwrap();
            let mut absent = event.clone();
            absent.event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAW".into();
            assert!(!visible(&absent).unwrap());
            fs::write(imports.join("batch.json"), b"broken").unwrap();
            assert!(visible(&event).is_err());
        }
    }

    #[test]
    fn a_read_only_handle_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store {
            read_only: true,
            ..test_store(dir.path())
        };
        let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let bytes = br#"{"event_type":"note-added"}"#;
        assert!(store.append_raw_event("change", event_id, bytes).is_err());
        assert!(store.append_raw_repository_event(event_id, bytes).is_err());
        assert!(!dir.path().join("changes").exists());
        assert!(!dir.path().join("repository").exists());
    }

    #[test]
    fn candidate_payloads_stamp_seven_through_typed_and_both_raw_paths() {
        let fixtures = [
            serde_json::json!({"event_type": "candidate-registered", "candidate_id": "candidate", "tree": "tree", "brief": {"change_id": "change", "brief_event_id": "version", "digest": "digest"}, "producers": ["producer"]}),
            serde_json::json!({"event_type": "candidate-judged", "candidate_id": "candidate", "judgement": {"kind": "rejected"}, "reason": "reason"}),
            serde_json::json!({"event_type": "candidate-retired", "candidate_id": "candidate"}),
            serde_json::json!({"event_type": "candidate-evidence-received", "candidate_id": "candidate", "evidence": {
                "schema": "arc-received-evidence/1", "run_handle": "run", "packet": "packet",
                "heads": {
                    "source": {"commit": "1".repeat(40), "tree": "2".repeat(40)},
                    "tested": {"commit": "1".repeat(40), "tree": "2".repeat(40)},
                    "report": {"commit": "3".repeat(40), "tree": "2".repeat(40)},
                },
                "applicability": {"applies_to": "tested"},
                "omissions": [],
            }}),
            serde_json::json!({"event_type": "candidate-verified", "candidate_id": "candidate", "tree": "tree", "gate": "test", "command": "true", "result": "pass", "hostname": "host"}),
            serde_json::json!({"event_type": "candidate-selected", "candidate_id": "candidate", "destination": "change", "head": "head", "target_branch": "main", "target": "target", "tree": "tree", "reuse": "never", "contributors": ["producer"], "selector": "selector", "rationale": "reason"}),
            serde_json::json!({"event_type": "candidate-promoted", "selection": "selection", "candidate_id": "candidate", "destination": "change", "patchset_id": "patchset", "revision": "revision"}),
        ];
        let mut mismatches = Vec::new();
        for value in fixtures {
            let payload: Payload = serde_json::from_value(value.clone()).unwrap();
            let serialized = serde_json::to_value(&payload).unwrap();
            assert_eq!(serialized["event_type"], value["event_type"]);
            for path in ["typed", "raw-change", "raw-repository"] {
                let dir = tempfile::tempdir().unwrap();
                let config = StoreConfig {
                    schema_version: 1,
                    repository_id: "repo".into(),
                    created_at: chrono::Utc::now(),
                };
                fs::write(
                    dir.path().join("config.json"),
                    serde_json::to_vec(&config).unwrap(),
                )
                .unwrap();
                let store = test_store(dir.path());
                let bytes = serde_json::to_vec(&value).unwrap();
                match path {
                    "typed" => store.stamp_format_for(&payload).unwrap(),
                    "raw-change" => {
                        store.append_raw_event("change", "event", &bytes).unwrap();
                    }
                    _ => {
                        store.append_raw_repository_event("event", &bytes).unwrap();
                    }
                }
                let stored: StoreConfig =
                    serde_json::from_slice(&fs::read(dir.path().join("config.json")).unwrap())
                        .unwrap();
                if stored.schema_version != 7 {
                    mismatches.push(format!(
                        "{} {path}: {}",
                        value["event_type"], stored.schema_version
                    ));
                }
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("; "));
    }

    #[test]
    fn format_stamp_waits_on_one_store_wide_lock_and_never_downgrades() {
        let dir = tempfile::tempdir().unwrap();
        let config = StoreConfig {
            schema_version: 1,
            repository_id: "repo".into(),
            created_at: chrono::Utc::now(),
        };
        fs::write(
            dir.path().join("config.json"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .unwrap();
        let held = test_store(dir.path()).lock_format().unwrap();
        let root = dir.path().to_path_buf();
        let (sent, received) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            test_store(&root).stamp_format(Some(3)).unwrap();
            sent.send(()).unwrap();
        });

        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        drop(held);
        received.recv_timeout(Duration::from_secs(1)).unwrap();
        writer.join().unwrap();
        test_store(dir.path()).stamp_format(Some(2)).unwrap();

        let stored: StoreConfig =
            serde_json::from_slice(&fs::read(dir.path().join("config.json")).unwrap()).unwrap();
        assert_eq!(stored.schema_version, 3);
    }
    fn open_change(store: &Store, change: &str) {
        let opening: Event = serde_json::from_value(serde_json::json!({
            "schema_version": crate::model::SCHEMA_VERSION, "event_id": ids::new_event_id().unwrap(), "repository_id":"repo", "change_id": change, "actor":"writer", "created_at":"2026-10-04T00:00:00Z",
            "event_type":"change-opened", "slug": change, "title":"Change", "profile":"local", "target_branch":"master", "branch": format!("arc/{change}"), "base":"base"
        })).unwrap();
        store.append_event(&opening).unwrap();
    }

    #[test]
    fn an_aggregate_read_derives_the_privilege_scope_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path());
        let changes = ["one", "two", "three", "four", "five"];
        for change in changes {
            open_change(&store, change);
        }
        let derivations = crate::import_authorization::scope_derivations;
        let before = derivations();
        assert_eq!(store.readable_states().unwrap().len(), changes.len());
        assert_eq!(derivations() - before, 1);

        let before = derivations();
        let scope = store.privilege_scope().unwrap();
        for change in changes {
            store.state_in(&scope, change).unwrap();
        }
        assert_eq!(
            derivations() - before,
            1,
            "reading a change with a held scope derives nothing"
        );
    }

    #[test]
    fn a_read_that_finds_no_change_derives_no_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path());
        let before = crate::import_authorization::scope_derivations();
        assert!(store.readable_states().unwrap().is_empty());
        assert_eq!(crate::import_authorization::scope_derivations(), before);
    }

    #[test]
    fn a_scope_is_derived_after_a_write_not_kept_across_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path());
        open_change(&store, "one");
        assert_eq!(store.readable_states().unwrap().len(), 1);
        open_change(&store, "two");
        let before = crate::import_authorization::scope_derivations();
        assert_eq!(store.readable_states().unwrap().len(), 2);
        assert_eq!(
            crate::import_authorization::scope_derivations() - before,
            1,
            "a read after a write derives afresh"
        );
    }

    #[test]
    fn append_refuses_an_event_before_the_change_opening_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(dir.path());
        let opening: Event = serde_json::from_value(serde_json::json!({
            "schema_version": crate::model::SCHEMA_VERSION, "event_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV", "repository_id":"repo", "change_id":"change", "actor":"writer", "created_at":"2026-10-04T00:00:00Z",
            "event_type":"change-opened", "slug":"change", "title":"Change", "profile":"local", "target_branch":"master", "branch":"arc/change", "base":"base"
        })).unwrap();
        store.append_event(&opening).unwrap();
        let mut earlier = opening.clone();
        earlier.event_id = "01ARZ3NDEKTSV4RRFFQ69G5FA0".into();
        earlier.payload = Payload::HoldSet {
            reason: "hold".into(),
        };
        let before = fs::read(dir.path().join("config.json")).unwrap_or_default();
        let error = store.append_event(&earlier).unwrap_err();
        assert!(format!("{error:#}").contains("opening"));
        assert_eq!(store.raw_events("change").unwrap().len(), 1);
        assert_eq!(
            fs::read(dir.path().join("config.json")).unwrap_or_default(),
            before
        );
    }
}
