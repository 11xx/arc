use crate::gitio;
use crate::ids;
use crate::model::{ActorSource, Event, Payload};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

// A known opening ID locates an immutable event directly when the directory
// becomes unlistable. The event bytes are still validated before any append.
static OPENING_IDS: std::sync::Mutex<BTreeMap<(PathBuf, String), String>> =
    std::sync::Mutex::new(BTreeMap::new());

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

/// A process-scoped transition guard. The lock file is intentionally
/// persistent: the OS releases the advisory lock when this handle is dropped
/// or the process exits, so a crash cannot leave a stale ownership marker.
pub struct TransitionLock(File);

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
        batches.push(batch);
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
                    repository_id: ids::new_event_id(),
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

    pub fn append_repository_event(&self, event: &Event) -> Result<()> {
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
        ids::validate_id_component(&event.event_id)
    }

    /// Repository-scoped events in ID order. A repository that has never had
    /// one has no directory, which is not an error.
    pub fn load_repository_events(&self) -> Result<Vec<Event>> {
        self.raw_repository_events_unseen(&BTreeSet::new())?
            .into_iter()
            .filter_map(|(_, value)| match serde_json::from_value::<Event>(value) {
                Ok(event) if matches!(event.payload, Payload::Unknown) => None,
                other => Some(other.context("malformed repository event")),
            })
            .collect()
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
        intent: &crate::commands::provisional::Intent,
    ) -> Result<()> {
        self.refuse_read_only()?;
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
        if !self.event_is_published(event)? {
            self.append_event(event)?;
        }
        let directory = self.events_dir(&event.change_id);
        File::open(directory.join(format!("{}.json", event.event_id)))?.sync_all()?;
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
    pub fn append_event(&self, event: &Event) -> Result<()> {
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
        self.event_is_published_at(event, &self.events_dir(&event.change_id))
    }

    pub(crate) fn repository_event_is_published(&self, event: &Event) -> Result<bool> {
        self.event_is_published_at(event, &self.repository_events_dir())
    }

    fn event_is_published_at(&self, event: &Event, directory: &Path) -> Result<bool> {
        ids::validate_id_component(&event.event_id)?;
        let path = directory.join(format!("{}.json", event.event_id));
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect {}", path.display()))
            }
        };
        let published: serde_json::Value = serde_json::from_slice(&bytes)?;
        if published != serde_json::to_value(event)? {
            bail!(
                "published event {} differs from the attempted event",
                event.event_id
            );
        }
        Ok(true)
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
        let temporary = config_path.with_extension(format!("json.{}", ids::new_event_id()));
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
        let mut state = crate::state::reduce_following(events, rewrites)?;
        state.apply_privilege_scope(
            crate::import_authorization::PrivilegeScope::for_store(self)?,
            events,
        );
        crate::replica::localize_change(&self.repository_id, events, &mut state);
        Ok(state)
    }

    pub fn state(&self, change_id: &str) -> Result<crate::state::ChangeState> {
        let events = self.load_events(change_id)?;
        let mut state = crate::state::reduce_following(&events, &self.rewrites()?)?;
        state.apply_privilege_scope(
            crate::import_authorization::PrivilegeScope::for_store(self)?,
            &events,
        );
        crate::replica::localize_change(&self.repository_id, &events, &mut state);
        Ok(state)
    }

    /// Readable changes for an aggregate view. A per-change failure is named
    /// on stderr; failures reading the shared ledger or rewrite map refuse
    /// the observation because they cannot be attributed to one change.
    pub fn readable_states(&self) -> Result<BTreeMap<String, crate::state::ChangeState>> {
        let rewrites = self.rewrites()?;
        let mut states = BTreeMap::new();
        for change_id in self.list_change_ids()? {
            let result = self.load_events(&change_id).and_then(|events| {
                let mut state = crate::state::reduce_following(&events, &rewrites)?;
                state.apply_privilege_scope(
                    crate::import_authorization::PrivilegeScope::for_store(self)?,
                    &events,
                );
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

    /// All events of one change in ULID (i.e. chronological) order.
    pub fn load_events(&self, change_id: &str) -> Result<Vec<Event>> {
        typed_events(self.raw_events(change_id)?)
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
        let dir = self.root.join("imports");
        create_private_dir_all(&dir).map_err(|error| crate::model::OperationFailure {
            published: false,
            message: format!("batch directory preparation failed: {error:#}"),
        })?;
        let path = dir.join(format!("{}.json", ids::new_event_id()));
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
        write_exclusive(&path, bytes)
            .with_context(|| format!("repository event {event_id} already exists during import"))?;
        Ok(true)
    }

    #[cfg(test)]
    pub fn append_raw_event(&self, change_id: &str, event_id: &str, bytes: &[u8]) -> Result<()> {
        self.refuse_read_only()?;
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

/// Durably publishes a new file by fsyncing its contents, hard-linking it into
/// place, and fsyncing the containing directory.
fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<()> {
    let file_name = path
        .file_name()
        .context("exclusive-write path has no file name")?
        .to_string_lossy();
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", crate::ids::new_event_id()));

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
