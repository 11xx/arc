use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// User-level configuration. arc treats `~/.local/ai/` as the AI data
/// home (relocatable via `AI_HOME`) and reads `<ai-home>/arc/config.toml`.
/// Environment variables override the file.
///
/// A sandbox prefix (`ARC_SANDBOX`, `--sandbox`) stands in for the home
/// directory everywhere arc derives a default path, so one value moves every
/// root arc writes at once. A variable naming one exact directory
/// (`AI_HOME`, `ARC_WORKTREES_DIR`, `ARC_DATA_ROOT`, `ARC_DATA_DIR`,
/// `ARC_JOURNAL_DIR`) still means the directory it names: the prefix replaces
/// defaults, not statements.
///
/// The prefix bounds recorded paths as well as derived ones. Under a sandbox
/// arc runs commands in, and writes beneath, the prefix and the repository it
/// was pointed at; a checkout some record names anywhere else is out of reach.
#[derive(Debug, Default, Deserialize)]
pub struct ConfigFile {
    /// Directory that receives change worktrees (default `~/.worktrees`). A
    /// relative value remains relative until the command using it resolves it
    /// against its invocation path.
    #[serde(default)]
    pub worktrees_dir: Option<String>,
    /// When set, ledgers live at `<data_root>/<repo-path-slug>/` instead
    /// of inside each repository's Git common dir.
    #[serde(default)]
    pub data_root: Option<String>,
    /// Stable absolute path scopes for project-journal directories. The
    /// longest matching component prefix wins before Git discovery; for a Git
    /// repository, matching uses its shared root so linked worktrees cannot
    /// select different journals. A prefix covering repositories shadows
    /// their Git journals. Values may use a leading `~`.
    #[serde(default)]
    pub journals: JournalsConfig,
    /// Journal behavior toggles (distinct from the per-project `[journals]`
    /// directory map).
    #[serde(default)]
    pub journal: JournalBehavior,
    /// Identity behavior toggles.
    #[serde(default)]
    pub identity: IdentityBehavior,
    /// Git provenance behavior.
    #[serde(default)]
    pub provenance: ProvenanceBehavior,
}

/// The `[journal]` table: opt-in behavior for the advisory journal.
#[derive(Debug, Default, Deserialize)]
pub struct JournalBehavior {
    /// When true, `begin`/`integrate`/`close` append a journal `log` event
    /// narrating the lifecycle transition. Advisory: a write failure is a
    /// warning, never a command failure.
    #[serde(default)]
    pub auto_log: bool,
}

/// The `[identity]` table: opt-in ambient identity resolution.
#[derive(Debug, Default, Deserialize)]
pub struct IdentityBehavior {
    /// When true, a command with no explicit harness/session/model falls
    /// back to detecting them from the running harness's own session store.
    #[serde(default)]
    pub detect: bool,
}

/// The `[provenance]` table: how ledger actors relate to Git identities.
#[derive(Debug, Default, Deserialize)]
pub struct ProvenanceBehavior {
    /// Whether each actor has its own Git identity or commits use a shared one.
    #[serde(default)]
    pub git_identity: GitIdentityMode,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GitIdentityMode {
    #[default]
    PerActor,
    Shared,
}

impl GitIdentityMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GitIdentityMode::PerActor => "per-actor",
            GitIdentityMode::Shared => "shared",
        }
    }
}

/// The `[journals]` table: a `dirs` map from stable path prefix to journal
/// directory. A dedicated table (rather than a general `[project."…"]`
/// namespace) keeps the override self-documenting and matches config.rs's
/// flat, single-purpose key idiom.
#[derive(Debug, Default, Deserialize)]
pub struct JournalsConfig {
    #[serde(default)]
    pub dirs: BTreeMap<String, String>,
}

#[derive(Debug)]
pub struct Config {
    /// The sandbox prefix every default path was derived from, when one is in
    /// force. `None` means the roots are the caller's own.
    pub sandbox: Option<PathBuf>,
    pub ai_home: PathBuf,
    pub config_path: PathBuf,
    pub worktrees_dir: PathBuf,
    pub data_root: Option<PathBuf>,
    /// Raw `[journals] dirs` map (stable path prefix -> journal directory),
    /// values not yet tilde-expanded (the consumer expands on lookup).
    pub journal_dirs: BTreeMap<String, String>,
    /// Whether lifecycle transitions append advisory journal log events.
    pub journal_auto_log: bool,
    /// Whether omitted identity fields fall back to ambient detection.
    pub identity_detect: bool,
    /// How ledger actors relate to Git author and committer identities.
    pub provenance_git_identity: GitIdentityMode,
}

/// The variable naming the sandbox prefix. The `--sandbox` flag exports it, so
/// a command arc runs (a gate, a hook, a nested `arc`) inherits the sandbox
/// rather than reaching back into the caller's own roots.
pub const SANDBOX_VAR: &str = "ARC_SANDBOX";

/// The sandbox prefix in force, when there is one.
///
/// An absolute path is required: every root is derived by joining onto it, and
/// a relative prefix would mean a different set of roots per working directory,
/// which is the opposite of what a sandbox is for.
pub fn sandbox() -> Result<Option<PathBuf>> {
    let Some(raw) = std::env::var_os(SANDBOX_VAR) else {
        return Ok(None);
    };
    let path = PathBuf::from(&raw);
    if path.as_os_str().is_empty() {
        return Ok(None);
    }
    let path = expand_tilde_at(&real_home()?, path.to_string_lossy().as_ref())?;
    if !path.is_absolute() {
        bail!(
            "{SANDBOX_VAR} must be an absolute path, got {}",
            path.display()
        );
    }
    Ok(Some(path))
}

/// The directory every default path is derived from: the sandbox prefix where
/// one is in force, otherwise the caller's home.
fn home() -> Result<PathBuf> {
    match sandbox()? {
        Some(prefix) => Ok(prefix),
        None => real_home(),
    }
}

/// The caller's own home directory, whatever a sandbox stands in for. Only a
/// check that has to distinguish the two asks for it.
pub fn real_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is unset")
}

pub fn ai_home() -> Result<PathBuf> {
    match std::env::var_os("AI_HOME") {
        Some(d) => Ok(PathBuf::from(d)),
        None => Ok(ai_home_under(&home()?)),
    }
}

/// The AI data home a home directory — or a sandbox prefix standing in for one
/// — supplies. One definition, so a sandbox built by hand and a sandbox
/// `ARC_SANDBOX` resolves to are the same layout.
pub fn ai_home_under(base: &Path) -> PathBuf {
    base.join(".local").join("ai")
}

/// The worktrees directory a home directory or sandbox prefix supplies.
pub fn worktrees_under(base: &Path) -> PathBuf {
    base.join(".worktrees")
}

/// Where arc puts a scratch directory it creates and removes itself. Inside a
/// sandbox this is part of the sandbox, so a run leaves nothing in the shared
/// temp directory either.
pub fn temp_dir() -> Result<PathBuf> {
    match sandbox()? {
        Some(prefix) => Ok(prefix.join("tmp")),
        None => Ok(std::env::temp_dir()),
    }
}

/// A path resolved as far as the filesystem can resolve it: the longest
/// existing ancestor canonicalized, with whatever does not exist appended.
///
/// Comparing two paths by spelling answers wrongly wherever a symlink stands
/// in either of them, and a recorded path that has since been removed still
/// has to compare against the roots it was recorded under.
pub fn resolved(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut at = path;
    loop {
        if let Ok(canonical) = std::fs::canonicalize(at) {
            return missing
                .iter()
                .rev()
                .fold(canonical, |resolved, part| resolved.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The sandbox prefix that puts `path` out of arc's reach, or `None` when arc
/// may run a command there and write beneath it.
///
/// Absent a sandbox everything is in reach. Under one, arc touches the prefix
/// and the repository it was pointed at, and nothing else. That bound has to
/// be applied to any path arc reads out of a record rather than deriving:
/// a ledger states absolute checkout paths, and a ledger copied into a sandbox
/// states the original's, so following one would put gates, replays, and
/// spooled writes back into the very checkout a sandbox exists to hold apart.
pub fn excluded_by_sandbox(repository: &Path, path: &Path) -> Result<Option<PathBuf>> {
    let Some(prefix) = sandbox()? else {
        return Ok(None);
    };
    let path = resolved(path);
    if path.starts_with(resolved(&prefix)) || path.starts_with(resolved(repository)) {
        return Ok(None);
    }
    Ok(Some(prefix))
}

/// Whether arc may run a command in `path` and write beneath it, for a caller
/// that has nothing to say when it may not.
pub fn admits_path(repository: &Path, path: &Path) -> bool {
    matches!(excluded_by_sandbox(repository, path), Ok(None))
}

/// Expand a leading `~` against the directory arc derives defaults from, so a
/// configured `~/…` path follows a sandbox prefix the same way a default does.
pub fn expand_tilde(s: &str) -> Result<PathBuf> {
    expand_tilde_at(&home()?, s)
}

fn expand_tilde_at(base: &Path, s: &str) -> Result<PathBuf> {
    if s == "~" {
        Ok(base.to_path_buf())
    } else if let Some(rest) = s.strip_prefix("~/") {
        Ok(base.join(rest))
    } else {
        Ok(PathBuf::from(s))
    }
}

pub fn load() -> Result<Config> {
    let sandbox = sandbox()?;
    let ai_home = ai_home()?;
    let config_path = ai_home.join("arc").join("config.toml");
    let file: ConfigFile = if config_path.is_file() {
        let text = std::fs::read_to_string(&config_path)
            .with_context(|| format!("cannot read {}", config_path.display()))?;
        toml::from_str(&text).with_context(|| format!("malformed {}", config_path.display()))?
    } else {
        ConfigFile::default()
    };

    let worktrees_dir = match std::env::var_os("ARC_WORKTREES_DIR") {
        Some(d) => PathBuf::from(d),
        None => match &file.worktrees_dir {
            Some(s) => expand_tilde(s)?,
            None => worktrees_under(&home()?),
        },
    };
    let data_root = match std::env::var_os("ARC_DATA_ROOT") {
        Some(d) => Some(PathBuf::from(d)),
        None => match &file.data_root {
            Some(s) => Some(expand_tilde(s)?),
            None => None,
        },
    };

    Ok(Config {
        sandbox,
        ai_home,
        config_path,
        worktrees_dir,
        data_root,
        journal_dirs: file.journals.dirs,
        journal_auto_log: file.journal.auto_log,
        identity_detect: file.identity.detect,
        provenance_git_identity: file.provenance.git_identity,
    })
}

/// Slug an absolute repository path the same way the project journal
/// keys projects: `/` and `.` become `-`.
pub fn path_slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_slug_matches_journal_convention() {
        assert_eq!(
            path_slug(Path::new("/home/lobo/code/muzaiten")),
            "-home-lobo-code-muzaiten"
        );
    }
}
