//! Directory handles that custody publishes through.
//!
//! A path is re-resolved by every call that receives it, so a directory
//! swapped for a symlink between a check and a write redirects the write. A
//! [`Dir`] is resolved once, refusing symlinks below its root, and every
//! later operation names an entry of that handle alone.
use anyhow::{Context, Result};
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// A directory a handle was asked to descend into is a symlink.
#[derive(Debug)]
pub struct Symlinked(pub PathBuf);

impl std::fmt::Display for Symlinked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is a symlink; custody stays inside the ledger and never follows one",
            self.0.display()
        )
    }
}

impl std::error::Error for Symlinked {}

#[cfg(any(target_os = "netbsd", target_os = "openbsd"))]
use libc::__errno as errno;
#[cfg(any(target_os = "linux", target_os = "android"))]
use libc::__errno_location as errno;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
use libc::__error as errno;

pub struct Dir {
    fd: OwnedFd,
    path: PathBuf,
}

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// One path component: no separator, no `.` or `..`.
fn component(name: &str) -> io::Result<CString> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name:?} is not a single path component"),
        ));
    }
    CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "embedded NUL"))
}

impl Dir {
    /// Open `path` itself. Its own final component may be a link: it names a
    /// root the caller already trusts.
    pub fn open_root(path: &Path) -> io::Result<Dir> {
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(path)?;
        Ok(Dir {
            fd: directory.into(),
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn open_at(&self, name: &CStr, flags: libc::c_int, mode: u32) -> io::Result<OwnedFd> {
        // SAFETY: `name` is a NUL-terminated string that outlives the call, and
        // the directory descriptor is open for as long as `self` is.
        let fd = check(unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        })?;
        // SAFETY: `openat` returned a descriptor nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn is_symlink(&self, name: &CStr) -> bool {
        // SAFETY: `stat` is plain data an all-zero pattern is valid for, and
        // `fstatat` fills it from a NUL-terminated name and an open descriptor.
        let mut status = unsafe { std::mem::zeroed::<libc::stat>() };
        let found = unsafe {
            libc::fstatat(
                self.fd.as_raw_fd(),
                name.as_ptr(),
                &mut status,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        found == 0 && status.st_mode & libc::S_IFMT == libc::S_IFLNK
    }

    /// The directory `name` under this one, never through a symlink. An
    /// absent one is `None`, or is created with `create` as its mode.
    pub fn descend(&self, name: &str, create: Option<u32>) -> Result<Option<Dir>> {
        let entry = component(name)?;
        let path = self.path.join(name);
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        let mut created = false;
        loop {
            match self.open_at(&entry, flags, 0) {
                Ok(fd) => return Ok(Some(Dir { fd, path })),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    // A dangling link reads as absent but is not an entry to
                    // create through.
                    if self.is_symlink(&entry) {
                        return Err(Symlinked(path).into());
                    }
                    let Some(mode) = create.filter(|_| !created) else {
                        return if created {
                            Err(error).with_context(|| format!("cannot open {}", path.display()))
                        } else {
                            Ok(None)
                        };
                    };
                    // SAFETY: as for `openat`.
                    match check(unsafe {
                        libc::mkdirat(self.fd.as_raw_fd(), entry.as_ptr(), mode as libc::mode_t)
                    }) {
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => {
                            return Err(error)
                                .with_context(|| format!("cannot create {}", path.display()))
                        }
                    }
                    created = true;
                }
                Err(error) => {
                    return if self.is_symlink(&entry) {
                        Err(Symlinked(path).into())
                    } else {
                        Err(error).with_context(|| format!("cannot open {}", path.display()))
                    }
                }
            }
        }
    }

    /// A new file that must not exist, and is not reached through a link.
    pub fn create_file(&self, name: &str, mode: u32) -> io::Result<File> {
        let entry = component(name)?;
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW;
        Ok(self.open_at(&entry, flags, mode)?.into())
    }

    /// An entry opened for reading without following a link or waiting on a
    /// writer. The caller still decides whether it is a regular file.
    pub fn open_entry(&self, name: &str) -> io::Result<File> {
        let entry = component(name)?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        Ok(self.open_at(&entry, flags, 0)?.into())
    }

    /// Whether an open failure was a refusal to follow a link.
    pub fn refused_link(error: &io::Error) -> bool {
        error.raw_os_error() == Some(libc::ELOOP)
    }

    /// Move an entry over another entry of this directory.
    pub fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let (from, to) = (component(from)?, component(to)?);
        // SAFETY: as for `openat`.
        check(unsafe {
            libc::renameat(
                self.fd.as_raw_fd(),
                from.as_ptr(),
                self.fd.as_raw_fd(),
                to.as_ptr(),
            )
        })
        .map(drop)
    }

    pub fn remove_file(&self, name: &str) -> io::Result<()> {
        let entry = component(name)?;
        // SAFETY: as for `openat`.
        check(unsafe { libc::unlinkat(self.fd.as_raw_fd(), entry.as_ptr(), 0) }).map(drop)
    }

    pub fn sync(&self) -> io::Result<()> {
        // SAFETY: the descriptor is open for as long as `self` is.
        check(unsafe { libc::fsync(self.fd.as_raw_fd()) }).map(drop)
    }

    /// The names in this directory, unordered.
    pub fn names(&self) -> io::Result<Vec<String>> {
        // SAFETY: the duplicate is a descriptor this function owns until
        // `fdopendir` takes it over, or `close` releases it when that fails.
        let duplicate = check(unsafe { libc::dup(self.fd.as_raw_fd()) })?;
        let stream = unsafe { libc::fdopendir(duplicate) };
        if stream.is_null() {
            let error = io::Error::last_os_error();
            unsafe { libc::close(duplicate) };
            return Err(error);
        }
        unsafe { libc::rewinddir(stream) };
        let mut names = Vec::new();
        loop {
            // SAFETY: `stream` is open until `closedir`; `errno` is reset so
            // the end of the stream is told from a failure.
            unsafe { *errno() = 0 };
            let entry = unsafe { libc::readdir(stream) };
            // SAFETY: a non-null entry's `d_name` is a NUL-terminated string
            // valid until the next `readdir`, and is copied out immediately.
            if entry.is_null() {
                let error = io::Error::last_os_error();
                unsafe { libc::closedir(stream) };
                return match error.raw_os_error() {
                    Some(0) | None => Ok(names),
                    Some(_) => Err(error),
                };
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_string_lossy();
            if name != "." && name != ".." {
                names.push(name.into_owned());
            }
        }
    }
}
