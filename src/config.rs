use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// User-level configuration. arc treats `~/.local/ai/` as the AI data
/// home (relocatable via `AI_HOME`) and reads `<ai-home>/arc/config.toml`.
/// Environment variables override the file; flags do not exist for these
/// on purpose — sandboxing is achieved by pointing the paths elsewhere.
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

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is unset")
}

pub fn ai_home() -> Result<PathBuf> {
    match std::env::var_os("AI_HOME") {
        Some(d) => Ok(PathBuf::from(d)),
        None => Ok(home()?.join(".local").join("ai")),
    }
}

pub fn expand_tilde(s: &str) -> Result<PathBuf> {
    if s == "~" {
        home()
    } else if let Some(rest) = s.strip_prefix("~/") {
        Ok(home()?.join(rest))
    } else {
        Ok(PathBuf::from(s))
    }
}

pub fn load() -> Result<Config> {
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
            None => home()?.join(".worktrees"),
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
