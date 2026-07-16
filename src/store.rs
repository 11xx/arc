use crate::gitio;
use crate::ids;
use crate::model::Event;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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
}

impl Store {
    /// Locate (creating on first use) the store for the repository
    /// containing `cwd`. Precedence: `ARC_DATA_DIR` (exact directory for
    /// exactly one repository) > configured `data_root` (per-repo slug
    /// subdirectory, sandbox-friendly) > the repository's Git common dir.
    pub fn discover(cwd: &Path) -> Result<Store> {
        let root = Self::resolve_root(cwd)?;
        create_private_dir(&root)?;
        let config_path = root.join("config.json");
        let repository_id = match fs::read(&config_path) {
            Ok(bytes) => {
                let cfg: StoreConfig =
                    serde_json::from_slice(&bytes).context("malformed arc config.json")?;
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
                cfg.repository_id
            }
        };
        Ok(Store {
            root,
            repository_id,
        })
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

    /// Append one event. The file is created exclusively; a collision on
    /// a ULID event ID indicates a real bug and fails loudly.
    pub fn append_event(&self, event: &Event) -> Result<()> {
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
            events.push(event);
        }
        Ok(events)
    }

    /// Raw JSON events of one change. Export deliberately bypasses the
    /// typed Event enum so future event types and fields survive intact.
    pub fn raw_events(&self, change_id: &str) -> Result<Vec<(String, serde_json::Value)>> {
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
            let path = entry.path();
            let value = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("malformed event file {}", path.display()))?;
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
                Ok(Some(cfg.repository_id))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn append_raw_event(&self, change_id: &str, event_id: &str, bytes: &[u8]) -> Result<()> {
        ids::validate_id_component(change_id)?;
        ids::validate_id_component(event_id)?;
        let dir = self.events_dir(change_id);
        create_private_dir_all(&dir)?;
        let path = dir.join(format!("{event_id}.json"));
        write_exclusive(&path, bytes)
            .with_context(|| format!("event {event_id} already exists during import"))
    }
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

fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
