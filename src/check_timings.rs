//! Bounded, advisory observations written by a gate command.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

pub const MAX_BYTES: usize = 65_536;
pub const MAX_ENTRIES: usize = 512;
pub const KEEP_ENTRIES: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub ms: u64,
    pub failed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Receipt {
    pub checks: Vec<Check>,
    pub dropped: usize,
    pub error: Option<String>,
}

type Stamp = (u64, u64, i64, i64, i64, i64);

fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::symlink_metadata(path).ok()?;
    Some((
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    ))
}

fn resolved(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = Path::new(name);
    if name.is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err("timings must name a relative file inside the worktree".into());
    }
    let path = root.join(path);
    let parent = path.parent().and_then(|p| p.canonicalize().ok());
    if !parent
        .zip(root.canonicalize().ok())
        .is_some_and(|(p, r)| p.starts_with(r))
    {
        return Err("timings parent is unavailable or outside the worktree".into());
    }
    Ok(path)
}

pub struct Pending {
    before: Option<Stamp>,
}

impl Pending {
    pub fn begin(root: &Path, name: &str) -> Self {
        let path = resolved(root, name);
        let before = path.as_ref().ok().and_then(|p| stamp(p));
        Self { before }
    }

    pub fn finish(self, root: &Path, name: &str) -> Receipt {
        let result = (|| {
            let path = resolved(root, name)?;
            if stamp(&path).is_some_and(|s| Some(s) == self.before) {
                return Err("timings file was not freshly written by the command".into());
            }
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)
                .map_err(|e| format!("cannot read timings: {e}"))?;
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("timings must be a regular file".into());
            }
            let mut bytes = Vec::new();
            file.take((MAX_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            parse(&bytes)
        })();
        result.unwrap_or_else(|error| Receipt {
            error: Some(error),
            ..Default::default()
        })
    }
}

fn parse(bytes: &[u8]) -> Result<Receipt, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!(
            "timings exceeds {MAX_BYTES} bytes; no checks retained"
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "timings is not UTF-8")?;
    let mut checks = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    let mut lines = text.lines();
    if lines.next() != Some("arc-check-timings/1") {
        return Err("missing arc-check-timings/1 header".into());
    }
    for (index, line) in lines.enumerate() {
        if index >= MAX_ENTRIES {
            return Err(format!(
                "timings exceeds {MAX_ENTRIES} entries; no checks retained"
            ));
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        let invalid = || {
            format!(
                "malformed timings line {}; expected name<TAB>milliseconds<TAB>pass|fail",
                index + 2
            )
        };
        if fields.len() != 3
            || fields[0].is_empty()
            || fields[0].len() > 128
            || fields[0].chars().any(char::is_control)
            || !names.insert(fields[0])
            || !matches!(fields[2], "pass" | "fail")
            || fields[1].is_empty()
            || !fields[1].bytes().all(|b| b.is_ascii_digit())
        {
            return Err(invalid());
        }
        checks.push(Check {
            name: fields[0].into(),
            ms: fields[1].parse().map_err(|_| invalid())?,
            failed: fields[2] == "fail",
        });
    }
    if checks.is_empty() {
        return Err("timings has no checks".into());
    }
    checks.sort_by(|a, b| {
        b.failed
            .cmp(&a.failed)
            .then(b.ms.cmp(&a.ms))
            .then(a.name.cmp(&b.name))
    });
    let dropped = checks.len().saturating_sub(KEEP_ENTRIES);
    checks.truncate(KEEP_ENTRIES);
    Ok(Receipt {
        checks,
        dropped,
        error: None,
    })
}
