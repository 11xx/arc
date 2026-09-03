use crate::gitio;
use crate::ids;
use crate::model::{ActorSource, Event, Payload};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

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
    pub root: PathBuf,
    pub repository_id: String,
    /// Whether this repository requires every writer to declare itself, read
    /// once when the store was opened from its repository.
    ///
    /// Reading it per append would let a command's own merge change the rule
    /// it is being judged by: `integrate` can bring in a commit that enables
    /// the policy, and the closure event would then be refused by a rule that
    /// did not exist when the merge was authorised.
    pub require_declared_actor: bool,
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

impl Store {
    /// Locate (creating on first use) the store for the repository
    /// containing `cwd`. Precedence: `ARC_DATA_DIR` (exact directory for
    /// exactly one repository) > configured `data_root` (per-repo slug
    /// subdirectory, sandbox-friendly) > the repository's Git common dir.
    pub fn discover(cwd: &Path) -> Result<Store> {
        let root = Self::resolve_root(cwd)?;
        // Read the repository's policy before creating anything, so an
        // unreadable one fails with the filesystem untouched.
        let require_declared_actor = match gitio::toplevel(cwd) {
            Ok(top) => crate::policy::load(&top)?.policy.require_declared_actor,
            // A store opened outside a repository has no policy to honour.
            Err(_) => false,
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
                    schema_version: crate::model::SCHEMA_VERSION,
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
            root,
            repository_id,
            require_declared_actor,
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
                root: root.to_path_buf(),
                repository_id,
                require_declared_actor: false,
            })),
            None => Ok(None),
        }
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
        self.refuse_undeclared_author(event)?;
        ids::validate_id_component(&event.event_id)?;
        let dir = self.repository_events_dir();
        create_private_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", event.event_id));
        let mut body = serde_json::to_vec_pretty(event)?;
        body.push(b'\n');
        write_exclusive(&path, &body)
            .with_context(|| format!("event {} already exists", event.event_id))
    }

    /// Repository-scoped events in ID order. A repository that has never had
    /// one has no directory, which is not an error.
    pub fn load_repository_events(&self) -> Result<Vec<Event>> {
        let dir = self.repository_events_dir();
        let mut names: Vec<String> = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", dir.display()))
            }
        };
        for entry in entries {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") {
                names.push(name);
            }
        }
        names.sort();
        let mut events = Vec::with_capacity(names.len());
        for name in names {
            let path = dir.join(&name);
            let bytes = fs::read(&path)?;
            let event: Event = serde_json::from_slice(&bytes)
                .with_context(|| format!("malformed event file {}", path.display()))?;
            if matches!(event.payload, crate::model::Payload::Unknown) {
                continue;
            }
            events.push(event);
        }
        Ok(events)
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

    pub fn list_change_ids(&self) -> Result<Vec<String>> {
        let dir = self.changes_dir();
        let mut ids_out = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return Ok(ids_out),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if ids::validate_id_component(&name).is_ok() {
                ids_out.push(name);
            }
        }
        ids_out.sort();
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
        self.refuse_undeclared_author(event)?;
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

    /// Record that this store now holds events an older build would skip.
    ///
    /// The format barrier only works if the store says which format it is. A
    /// ledger created by an older arc keeps its stamp until something writes
    /// an event that older arc would not understand — and a skipped lifecycle
    /// event is how a closed change gets read as open and closed a second way.
    /// Stamping at that moment, rather than on every open, means a build that
    /// only read a ledger never locks its owner out of it.
    fn stamp_format_for(&self, payload: &Payload) -> Result<()> {
        let introduced_in = match payload {
            Payload::ChangeIntegrated {
                authorization: Some(authorization),
                ..
            } if authorization.verdict_event_id.is_none() => Some(3),
            Payload::ChangeIntegrated { .. } | Payload::IntegrationAsserted { .. } => Some(2),
            _ => None,
        };
        self.stamp_format(introduced_in)
    }

    /// The same stamp, decided from raw imported JSON, so the import path and
    /// the typed record path cannot disagree about a wire-format addition.
    fn stamp_format_for_value(&self, value: Option<&serde_json::Value>) -> Result<()> {
        let introduced_in = match value
            .and_then(|value| value.get("event_type"))
            .and_then(serde_json::Value::as_str)
        {
            Some("change-integrated")
                if value
                    .and_then(|value| value.get("authorization"))
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(authorization_has_no_verdict) =>
            {
                Some(3)
            }
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
        // so a stale format-2 writer cannot land after a format-3 writer.
        let _format = self.lock_format()?;
        let config_path = self.root.join("config.json");
        let mut cfg: StoreConfig = serde_json::from_slice(&fs::read(&config_path)?)
            .context("malformed arc config.json")?;
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
            "policy requires a declared actor: {:?} on event {} was not declared by anyone.              Pass --actor or set ARC_ACTOR.",
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
    /// Recorded rewrites that contradict each other resolve nothing rather
    /// than making every read fail; `arc doctor` reports the contradiction as
    /// `invalid-rewrite-mapping`.
    pub fn state(&self, change_id: &str) -> Result<crate::state::ChangeState> {
        crate::state::reduce_following(&self.load_events(change_id)?, &self.rewrites())
    }

    /// Every history rewrite this repository recorded, flattened into one
    /// old-to-fate mapping. For readers that hold events already and still
    /// need the repository's own terms.
    pub fn rewrites(&self) -> crate::rewrite::RewriteMap {
        crate::rewrite::RewriteMap::load(self).unwrap_or_default()
    }

    /// All events of one change in ULID (i.e. chronological) order.
    pub fn load_events(&self, change_id: &str) -> Result<Vec<Event>> {
        ids::validate_id_component(change_id)?;
        let dir = self.events_dir(change_id);
        let mut names: Vec<String> = Vec::new();
        let entries =
            fs::read_dir(&dir).with_context(|| format!("unknown change {change_id:?}"))?;
        for entry in entries {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") {
                names.push(name);
            }
        }
        names.sort();
        let mut events = Vec::with_capacity(names.len());
        for name in names {
            let path = dir.join(&name);
            let bytes = fs::read(&path)?;
            let event: Event = serde_json::from_slice(&bytes)
                .with_context(|| format!("malformed event file {}", path.display()))?;
            // Skip (but never drop) events whose type this build does not
            // recognize: the file stays on disk and raw export round-trips it,
            // while typed replay tolerates a change that carries future events.
            if matches!(event.payload, crate::model::Payload::Unknown) {
                continue;
            }
            events.push(event);
        }
        Ok(events)
    }

    /// Raw JSON events of one change. Export deliberately bypasses the
    /// typed Event enum so future event types and fields survive intact.
    pub fn raw_events(&self, change_id: &str) -> Result<Vec<(String, serde_json::Value)>> {
        self.raw_events_unseen(change_id, &BTreeSet::new())
    }

    /// Read only raw events whose IDs the caller has not already observed.
    /// Follow mode uses this to rescan directory entries without reparsing the
    /// complete ledger on every poll.
    pub fn raw_events_unseen(
        &self,
        change_id: &str,
        seen: &BTreeSet<String>,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        ids::validate_id_component(change_id)?;
        let dir = self.events_dir(change_id);
        let entries =
            fs::read_dir(&dir).with_context(|| format!("unknown change {change_id:?}"))?;
        let mut events = Vec::new();
        for entry in entries {
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
        events.sort_by(|a, b| a.0.cmp(&b.0));
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
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", dir.display()))
            }
        };
        let mut files = Vec::new();
        for entry in entries {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let Some(event_id) = name.strip_suffix(".json") else {
                continue;
            };
            files.push((event_id.to_string(), fs::read(dir.join(&name))?));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(files)
    }

    /// Repository-scoped raw events not already seen, in ID order.
    pub fn raw_repository_events_unseen(
        &self,
        seen: &BTreeSet<String>,
    ) -> Result<Vec<(String, serde_json::Value)>> {
        let dir = self.repository_events_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", dir.display()))
            }
        };
        let mut events = Vec::new();
        for entry in entries {
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
        events.sort_by(|a, b| a.0.cmp(&b.0));
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
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
        let path = self
            .repository_events_dir()
            .join(format!("{event_id}.json"));
        if !path.exists() {
            return Ok(false);
        }
        let existing: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        let incoming: serde_json::Value = serde_json::from_slice(bytes)?;
        Ok(existing != incoming)
    }

    /// Write a repository-scoped event verbatim, as import does for a change.
    /// Idempotent: a rewrite already recorded here is the same fact.
    pub fn append_raw_repository_event(&self, event_id: &str, bytes: &[u8]) -> Result<bool> {
        ids::validate_id_component(event_id)?;
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
            if existing != incoming {
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

    pub fn append_raw_event(&self, change_id: &str, event_id: &str, bytes: &[u8]) -> Result<()> {
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
            root: root.to_path_buf(),
            repository_id: "repo".into(),
            require_declared_actor: false,
        }
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
}
