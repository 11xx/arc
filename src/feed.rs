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

use anyhow::{Context, Result};
use std::io::{Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::MetadataExt;
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

/// The feed file, followed from where the watch started across rotation.
pub(crate) struct Feed {
    dir: PathBuf,
    inotify: OwnedFd,
    /// The feed file being read, or `None` before the first notice ever.
    file: Option<std::fs::File>,
    /// The start of a line whose end has not been read yet.
    carry: Vec<u8>,
}

impl Feed {
    pub(crate) fn fd(&self) -> RawFd {
        self.inotify.as_raw_fd()
    }

    pub(crate) fn open() -> Result<Feed> {
        let dir = crate::feed::dir()
            .context("the AI home cannot be resolved, so there is no workspace feed to follow")?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create the workspace feed {}", dir.display()))?;
        // SAFETY: `inotify_init1` takes flags only and returns a new descriptor.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("cannot start inotify");
        }
        // SAFETY: `fd` is a descriptor this process just opened and owns.
        let inotify = unsafe { OwnedFd::from_raw_fd(fd) };
        let path = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes())
            .context("the workspace feed's path holds a NUL byte")?;
        // SAFETY: both arguments are valid for the call; the watch lives as
        // long as the descriptor.
        let watched = unsafe {
            libc::inotify_add_watch(
                inotify.as_raw_fd(),
                path.as_ptr(),
                libc::IN_MODIFY | libc::IN_CREATE | libc::IN_MOVED_TO,
            )
        };
        if watched < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("cannot watch {}", dir.display()));
        }
        let file = match std::fs::File::open(dir.join(crate::feed::FILE)) {
            Ok(mut file) => {
                file.seek(std::io::SeekFrom::End(0))?;
                Some(file)
            }
            Err(_) => None,
        };
        Ok(Feed {
            dir,
            inotify,
            file,
            carry: Vec::new(),
        })
    }

    /// Discard the queued inotify events: they only say the feed moved.
    pub(crate) fn drain_wakeups(&self) {
        let mut buffer = [0u8; 4096];
        // SAFETY: the buffer is valid for its length; the descriptor is
        // non-blocking, so the loop ends when the queue is empty.
        while unsafe {
            libc::read(
                self.inotify.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        } > 0
        {}
    }

    /// Every whole line appended since the last call, and whether a line was
    /// lost. When the feed has moved aside, the rest of the old file is read
    /// before the new one.
    pub(crate) fn lines(&mut self) -> (Vec<Vec<u8>>, bool) {
        let mut read = Vec::new();
        let mut lost = false;
        let path = self.dir.join(crate::feed::FILE);
        loop {
            if let Some(file) = &mut self.file {
                let _ = file.read_to_end(&mut self.carry);
            }
            let ours = self
                .file
                .as_ref()
                .and_then(|file| file.metadata().ok())
                .map(|metadata| (metadata.dev(), metadata.ino()));
            let current = std::fs::metadata(&path)
                .ok()
                .map(|metadata| (metadata.dev(), metadata.ino()));
            if current.is_none() || current == ours {
                break;
            }
            // The feed moved aside, and whatever the old file ended with
            // without a line end was cut off.
            if !self.carry.is_empty() && !self.carry.ends_with(b"\n") {
                self.carry.clear();
                lost = true;
            }
            self.file = std::fs::File::open(&path).ok();
        }
        let whole = self.carry.iter().rposition(|byte| *byte == b'\n');
        if let Some(end) = whole {
            let rest = self.carry.split_off(end + 1);
            let complete = std::mem::replace(&mut self.carry, rest);
            read.extend(
                complete
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                    .map(<[u8]>::to_vec),
            );
        }
        (read, lost)
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
