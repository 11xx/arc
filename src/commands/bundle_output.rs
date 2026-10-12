//! The artifacts an export writes: a bundle, an object list and a custody
//! sidecar, published together or not at all.
//!
//! Every artifact is finished beside its destination first, so a failure
//! while preparing any of them leaves the destinations untouched. Publishing
//! begins with the sidecar, because a bundle binds its manifest, and a failure
//! at any later step takes back what was published and restores what it
//! replaced.
use crate::custody::{Published, Staged};
use anyhow::{bail, ensure, Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `path` made absolute, with every existing component's links resolved and
/// the rest cleaned lexically, so that two spellings of one location compare
/// equal.
fn resolve(path: &Path) -> Result<PathBuf> {
    let absolute =
        std::path::absolute(path).with_context(|| format!("cannot resolve {}", path.display()))?;
    let mut existing = absolute.clone();
    let mut tail = Vec::new();
    let mut resolved = loop {
        match fs::canonicalize(&existing) {
            Ok(found) => break found,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name().map(|name| name.to_os_string()) else {
                    // A trailing `..` names no file: resolve what precedes it.
                    ensure!(existing.pop(), "cannot resolve {}", path.display());
                    tail.push(std::ffi::OsString::from(".."));
                    continue;
                };
                tail.push(name);
                existing.pop();
            }
            Err(error) => {
                return Err(error).with_context(|| format!("cannot resolve {}", path.display()))
            }
        }
    };
    for name in tail.into_iter().rev() {
        if name == ".." {
            resolved.pop();
        } else {
            resolved.push(name);
        }
    }
    Ok(resolved)
}

/// Refuse destinations that name the same location or one inside another,
/// and file destinations that are directories, before anything is written.
pub(super) fn check_destinations(files: &[(&str, &Path)], sidecar: Option<&Path>) -> Result<()> {
    let mut named = Vec::new();
    for (flag, path) in files {
        let resolved = resolve(path)?;
        ensure!(
            !resolved.is_dir(),
            "{flag} {} is a directory, not a file",
            path.display()
        );
        named.push((*flag, *path, resolved));
    }
    if let Some(path) = sidecar {
        named.push(("--custody-dir", path, resolve(path)?));
    }
    for (index, (first_flag, first, first_path)) in named.iter().enumerate() {
        for (second_flag, second, second_path) in &named[index + 1..] {
            if first_path.starts_with(second_path) || second_path.starts_with(first_path) {
                bail!(
                    "export destinations overlap: {first_flag} {} and {second_flag} {} \
                     would share a location; give each its own",
                    first.display(),
                    second.display()
                );
            }
        }
    }
    Ok(())
}

/// A file finished beside its destination and not yet in place. A file that
/// is not regular, such as a device, is written in place when published and
/// cannot be restored.
pub(super) struct StagedFile {
    destination: PathBuf,
    bytes: Option<Vec<u8>>,
    temporary: Option<PathBuf>,
    backup: Option<PathBuf>,
    published: bool,
}

impl StagedFile {
    pub(super) fn stage(path: &Path, bytes: &[u8]) -> Result<StagedFile> {
        let destination = match fs::canonicalize(path) {
            Ok(found) => found,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
            Err(error) => return Err(error.into()),
        };
        let held = match fs::metadata(&destination) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut staged = StagedFile {
            destination,
            bytes: None,
            temporary: None,
            backup: None,
            published: false,
        };
        if held.as_ref().is_some_and(|metadata| !metadata.is_file()) {
            staged.bytes = Some(bytes.to_vec());
            return Ok(staged);
        }
        let name = staged
            .destination
            .file_name()
            .context("destination has no file name")?
            .to_string_lossy()
            .into_owned();
        let sibling = |suffix: &str| {
            staged
                .destination
                .with_file_name(format!(".{name}.{}.{suffix}", crate::ids::new_identifier()))
        };
        let (temporary, backup) = (sibling("tmp"), sibling("bak"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        staged.temporary = Some(temporary);
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Some(held) = &held {
            fs::set_permissions(
                staged.temporary.as_ref().expect("staged"),
                held.permissions(),
            )?;
            fs::hard_link(&staged.destination, &backup)
                .or_else(|_| fs::copy(&staged.destination, &backup).map(drop))?;
            staged.backup = Some(backup);
        }
        Ok(staged)
    }

    fn publish(&mut self) -> Result<()> {
        if let Some(bytes) = &self.bytes {
            fs::write(&self.destination, bytes)?;
        } else if let Some(temporary) = &self.temporary {
            fs::rename(temporary, &self.destination)?;
            self.temporary = None;
        }
        self.published = true;
        Ok(())
    }

    /// Put back what the destination held before it was published.
    fn restore(&mut self) -> Result<()> {
        if !self.published || self.bytes.is_some() {
            return Ok(());
        }
        match self.backup.take() {
            Some(backup) => fs::rename(backup, &self.destination)?,
            None => fs::remove_file(&self.destination)?,
        }
        self.published = false;
        Ok(())
    }

    fn settle(&mut self) {
        if let Some(backup) = self.backup.take() {
            let _ = fs::remove_file(backup);
        }
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if let Some(temporary) = &self.temporary {
            let _ = fs::remove_file(temporary);
        }
        if !self.published {
            if let Some(backup) = &self.backup {
                let _ = fs::remove_file(backup);
            }
        }
    }
}

/// What an export publishes.
pub(super) struct Publication {
    pub(super) sidecar: Option<Staged>,
    pub(super) files: Vec<(String, StagedFile)>,
    pub(super) stdout: Option<Vec<u8>>,
}

impl Publication {
    /// Publish the sidecar, then each file, then the standard output stream.
    /// A failure restores every destination to what it held.
    pub(super) fn commit(mut self) -> Result<()> {
        let sidecar = self.sidecar.take().map(Staged::publish).transpose()?;
        match self.write_files() {
            Ok(()) => {
                self.files.iter_mut().for_each(|(_, file)| file.settle());
                Ok(())
            }
            Err(error) => Err(self.undo(sidecar, error)),
        }
    }

    fn write_files(&mut self) -> Result<()> {
        for (what, file) in &mut self.files {
            file.publish().with_context(|| what.clone())?;
        }
        if let Some(bytes) = &self.stdout {
            let mut out = std::io::stdout().lock();
            out.write_all(bytes)?;
            out.flush()?;
        }
        Ok(())
    }

    fn undo(&mut self, sidecar: Option<Published>, error: anyhow::Error) -> anyhow::Error {
        let mut failures = Vec::new();
        for (_, file) in self.files.iter_mut().rev() {
            if let Err(failure) = file.restore() {
                failures.push(format!("{failure:#}"));
            }
        }
        if let Some(Err(failure)) = sidecar.map(Published::withdraw) {
            failures.push(format!("{failure:#}"));
        }
        if failures.is_empty() {
            error
        } else {
            error.context(format!(
                "the export could not be fully undone: {}",
                failures.join("; ")
            ))
        }
    }
}
