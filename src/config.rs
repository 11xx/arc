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
    /// Directory that receives change worktrees (default `~/.worktrees`).
    #[serde(default)]
    pub worktrees_dir: Option<String>,
    /// When set, ledgers live at `<data_root>/<repo-path-slug>/` instead
    /// of inside each repository's Git common dir.
    #[serde(default)]
    pub data_root: Option<String>,
    /// Per-project overrides for the `/thread` archive directory. Keyed by
    /// the absolute repository-root path (the main checkout, shared by all
    /// its worktrees); values may use a leading `~`.
    #[serde(default)]
    pub threads: ThreadsConfig,
}

/// The `[threads]` table: a `dirs` map from repository-root path to archive
/// directory. A dedicated table (rather than a general `[project."…"]`
/// namespace) keeps the override self-documenting and matches config.rs's
/// flat, single-purpose key idiom.
#[derive(Debug, Default, Deserialize)]
pub struct ThreadsConfig {
    #[serde(default)]
    pub dirs: BTreeMap<String, String>,
}

#[derive(Debug)]
pub struct Config {
    pub ai_home: PathBuf,
    pub config_path: PathBuf,
    pub worktrees_dir: PathBuf,
    pub data_root: Option<PathBuf>,
    /// Raw `[threads] dirs` map (repository-root path -> archive dir),
    /// values not yet tilde-expanded (the consumer expands on lookup).
    pub thread_dirs: BTreeMap<String, String>,
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
        thread_dirs: file.threads.dirs,
    })
}

/// Slug an absolute repository path the same way the /thread archive
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
    fn path_slug_matches_thread_convention() {
        assert_eq!(
            path_slug(Path::new("/home/lobo/code/muzaiten")),
            "-home-lobo-code-muzaiten"
        );
    }
}
