//! Private combined gate diagnostics, bounded per run and per ledger.

use crate::custody::anchored::Dir;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const RETAINED_RUNS: usize = 16;
const TAIL_LIMIT: usize = 4096;

/// Capture metadata stays with its verification event when the local file expires.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputArtifact {
    pub producing_store: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub sha256: String,
    pub byte_length: u64,
    pub observed_sha256: String,
    pub observed_bytes: u64,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_error: Option<String>,
}

struct Storage {
    directory: Dir,
    file: File,
    temporary: String,
    name: String,
}

impl Storage {
    fn new(root: &Path) -> Result<Self> {
        let directory = Dir::open_root(root)?
            .descend("gate-output", Some(0o700))?
            .context("gate output directory unavailable")?;
        let name = format!("{}.log", crate::ids::new_identifier());
        let temporary = format!("{name}.{}.partial", std::process::id());
        let file = directory.create_file(&temporary, 0o600)?;
        Ok(Self {
            directory,
            file,
            temporary,
            name,
        })
    }

    fn publish(&self) -> Result<String> {
        self.file.sync_all()?;
        let lock = match self.directory.create_file("retention.lock", 0o600) {
            Ok(lock) => lock,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.directory.open_entry("retention.lock")?
            }
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            lock.metadata()?.is_file(),
            "gate retention lock is not a file"
        );
        // SAFETY: the open file owns the descriptor throughout publication and pruning.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        self.directory.rename(&self.temporary, &self.name)?;
        for name in self.directory.names()? {
            let Some((id, pid)) = name
                .strip_suffix(".partial")
                .and_then(|name| name.rsplit_once(".log."))
            else {
                continue;
            };
            let Ok(pid) = pid.parse::<i32>() else {
                continue;
            };
            if pid <= 0 || id.parse::<ulid::Ulid>().is_err() {
                continue;
            }
            // SAFETY: signal zero only tests whether the named positive process exists.
            if unsafe { libc::kill(pid, 0) } == -1
                && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                if self.directory.open_entry(&name)?.metadata()?.is_file() {
                    self.directory.remove_file(&name)?;
                }
            }
        }
        let mut names: Vec<_> = self
            .directory
            .names()?
            .into_iter()
            .filter(|name| {
                name.strip_suffix(".log")
                    .is_some_and(|id| id.parse::<ulid::Ulid>().is_ok())
            })
            .collect();
        names.sort();
        let excess = names.len().saturating_sub(RETAINED_RUNS);
        for name in names.iter().filter(|name| **name != self.name).take(excess) {
            let entry = self.directory.open_entry(name)?;
            if entry.metadata()?.is_file() {
                self.directory.remove_file(name)?;
            }
        }
        self.directory.sync()?;
        Ok(format!("gate-output/{}", self.name))
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        let _ = self.directory.remove_file(&self.temporary);
    }
}

pub struct Capture {
    storage: Option<Storage>,
    error: Option<String>,
    producing_store: String,
}

impl Capture {
    pub fn new(root: &Path, producing_store: &str) -> Self {
        let (storage, error) = match Storage::new(root) {
            Ok(storage) => (Some(storage), None),
            Err(error) => (
                None,
                Some(crate::session_store::bounded_reason(&format!("{error:#}"))),
            ),
        };
        Self {
            storage,
            error,
            producing_store: producing_store.to_owned(),
        }
    }

    #[cfg(test)]
    pub fn read(self, output: impl Read) -> io::Result<(Vec<u8>, OutputArtifact)> {
        self.read_bounded(output, OUTPUT_LIMIT)
    }

    pub fn read_with_notify(
        self,
        output: impl Read,
        drained: impl FnOnce(),
    ) -> io::Result<(Vec<u8>, OutputArtifact)> {
        self.read_with_limit(output, OUTPUT_LIMIT, drained)
    }

    #[cfg(test)]
    fn read_bounded(
        self,
        output: impl Read,
        limit: usize,
    ) -> io::Result<(Vec<u8>, OutputArtifact)> {
        self.read_with_limit(output, limit, || {})
    }

    fn read_with_limit(
        mut self,
        mut output: impl Read,
        limit: usize,
        drained: impl FnOnce(),
    ) -> io::Result<(Vec<u8>, OutputArtifact)> {
        let mut tail = Vec::with_capacity(TAIL_LIMIT);
        let mut retained = Sha256::new();
        let mut observed = Sha256::new();
        let mut length = 0_u64;
        let mut total = 0_u64;
        let mut chunk = [0_u8; 8192];
        loop {
            let count = output.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            observed.update(&chunk[..count]);
            total = total.saturating_add(count as u64);
            if self.error.is_none() {
                let keep = count.min(limit.saturating_sub(length as usize));
                if let Some(storage) = self.storage.as_mut() {
                    let mut written = 0;
                    while written < keep {
                        match storage.file.write(&chunk[written..keep]) {
                            Ok(0) => {
                                self.error = Some("gate output write made no progress".into());
                                break;
                            }
                            Ok(count) => {
                                retained.update(&chunk[written..written + count]);
                                length += count as u64;
                                written += count;
                            }
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                            Err(error) => {
                                self.error = Some(error.to_string());
                                break;
                            }
                        }
                    }
                }
            }
            tail.extend_from_slice(&chunk[..count]);
            if tail.len() > TAIL_LIMIT {
                tail.drain(..tail.len() - TAIL_LIMIT);
            }
        }
        drained();
        let path = if self.error.is_none() {
            match self.storage.as_ref().map(Storage::publish).transpose() {
                Ok(path) => path,
                Err(error) => {
                    self.error = Some(crate::session_store::bounded_reason(&format!("{error:#}")));
                    None
                }
            }
        } else {
            None
        };
        Ok((
            tail,
            OutputArtifact {
                producing_store: self.producing_store,
                path,
                sha256: hex::encode(retained.finalize()),
                byte_length: length,
                observed_sha256: hex::encode(observed.finalize()),
                observed_bytes: total,
                truncated: length < total,
                capture_error: self.error,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn capture_keeps_exact_bytes_and_bounds_the_file_without_losing_the_tail() {
        let root = tempfile::tempdir().unwrap();
        let bytes = [vec![0xff; 8192], b"last diagnostic".to_vec()].concat();
        let (tail, output) = Capture::new(root.path(), "repo")
            .read_bounded(bytes.as_slice(), 32)
            .unwrap();
        let path = root.path().join(output.path.unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), bytes[..32]);
        assert_eq!(output.sha256, hex::encode(Sha256::digest(&bytes[..32])));
        assert_eq!(output.observed_sha256, hex::encode(Sha256::digest(&bytes)));
        assert_eq!(output.observed_bytes, bytes.len() as u64);
        assert!(output.truncated);
        assert_eq!(tail, bytes[bytes.len() - TAIL_LIMIT..]);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(root.path().join("gate-output"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn retention_expires_only_completed_outputs() {
        let root = tempfile::tempdir().unwrap();
        let pending = Capture::new(root.path(), "repo");
        let mut first = None;
        for _ in 0..RETAINED_RUNS + 1 {
            let (_, output) = Capture::new(root.path(), "repo")
                .read(&b"diagnostic"[..])
                .unwrap();
            first.get_or_insert(output.path.unwrap());
        }
        assert!(!root.path().join(first.unwrap()).exists());
        let names = std::fs::read_dir(root.path().join("gate-output"))
            .unwrap()
            .count();
        assert_eq!(names, RETAINED_RUNS + 2);
        let (_, output) = pending.read(&b"pending"[..]).unwrap();
        assert!(output.capture_error.is_none());
    }

    #[test]
    fn a_linked_output_directory_records_capture_failure_and_still_drains() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        symlink(other.path(), root.path().join("gate-output")).unwrap();
        let (tail, output) = Capture::new(root.path(), "repo")
            .read(&b"diagnostic"[..])
            .unwrap();
        assert_eq!(tail, b"diagnostic");
        assert!(output.path.is_none());
        assert!(output.capture_error.is_some());
        assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 0);
    }
}
