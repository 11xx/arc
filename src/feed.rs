//! The workspace feed. After each committed ledger or journal write, arc
//! appends one line to `<ai-home>/arc/feed/events.jsonl` naming what was
//! written. A follower such as `arc workspace watch` learns from it which
//! project to read again, without watching every ledger and journal
//! directory itself.
//!
//! A notice follows its write and never stands in for it. Failing to append
//! one fails nothing, and a follower that missed notices reads everything
//! again. Past `ROTATE_AT` bytes the file moves aside to `events.jsonl.1`,
//! replacing the one before, so the feed holds at most two files.

use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) const SCHEMA: &str = "arc-workspace-feed/1";
pub(crate) const FILE: &str = "events.jsonl";
pub(crate) const PREVIOUS: &str = "events.jsonl.1";
const ROTATE_LOCK: &str = "rotate.lock";
const ROTATE_AT: u64 = 1 << 20;

/// The feed's directory, or `None` when the AI home cannot be resolved. The
/// crate's own unit tests write ledgers in scratch directories under no
/// sandbox, so they have no feed: their notices would land in the
/// operator's.
pub(crate) fn dir() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        crate::config::ai_home()
            .ok()
            .map(|home| home.join("arc").join("feed"))
    })
    .clone()
}

/// A ledger file was published at `path`. The notice names the change
/// when the file belongs to one.
pub(crate) fn ledger_written(path: &Path) {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut notice = serde_json::json!({
        "schema": SCHEMA,
        "at": now(),
        "source": "ledger",
        "path": path,
    });
    if let Some(change) = change_of(&path) {
        notice["change"] = change.into();
    }
    append(&notice);
}

/// A journal transaction committed in `dir`.
pub(crate) fn journal_written(dir: &Path) {
    append(&journal_notice(dir));
}

/// A binding was written to the journal in `dir`, which changes what the
/// registry says of it: the notice carries `binding`.
pub(crate) fn binding_written(dir: &Path) {
    let mut notice = journal_notice(dir);
    notice["binding"] = true.into();
    append(&notice);
}

fn journal_notice(dir: &Path) -> serde_json::Value {
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    serde_json::json!({
        "schema": SCHEMA,
        "at": now(),
        "source": "journal",
        "dir": dir,
    })
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The change whose event a ledger path is: `changes/<change>/events/<file>`,
/// read from the end so that no directory above the ledger can match.
fn change_of(path: &Path) -> Option<String> {
    let parts: Vec<&std::ffi::OsStr> = path.iter().collect();
    match parts.as_slice() {
        [.., changes, change, events, _] if *changes == "changes" && *events == "events" => {
            change.to_str().map(str::to_string)
        }
        _ => None,
    }
}

/// Append one notice as a single write, which `O_APPEND` keeps whole among
/// concurrent writers, then rotate when the file has grown past its bound.
fn append(notice: &serde_json::Value) {
    let Some(dir) = dir() else {
        return;
    };
    let mut line = notice.to_string();
    line.push('\n');
    let open = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(FILE))
    };
    let file = open().or_else(|_| {
        std::fs::create_dir_all(&dir)?;
        open()
    });
    let Ok(mut file) = file else {
        return;
    };
    if file.write_all(line.as_bytes()).is_err() {
        return;
    }
    if file
        .metadata()
        .is_ok_and(|metadata| metadata.len() > ROTATE_AT)
    {
        rotate(&dir);
    }
}

/// Move the feed aside once, whichever writer gets there first: the others
/// find the lock held, or the file already small, and leave it.
fn rotate(dir: &Path) {
    use std::os::fd::AsRawFd;
    let Ok(lock) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join(ROTATE_LOCK))
    else {
        return;
    };
    // SAFETY: `flock` reads only the descriptor, which `lock` keeps open; the
    // lock is released when `lock` closes.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return;
    }
    let current = dir.join(FILE);
    if std::fs::metadata(&current).is_ok_and(|metadata| metadata.len() > ROTATE_AT) {
        let _ = std::fs::rename(&current, dir.join(PREVIOUS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_file_names_its_change() {
        let root = Path::new("/work/changes/repo/.git/arc");
        assert_eq!(
            change_of(&root.join("changes/c-1/events/E1.json")).as_deref(),
            Some("c-1")
        );
        assert_eq!(change_of(&root.join("repository/events/E2.json")), None);
        assert_eq!(change_of(&root.join("provisional/c-1.json")), None);
    }
}
