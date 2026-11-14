//! Filesystem acts relative to an open directory that never follow a link
//! in the name they are given, so a check and the act it guards see one
//! object.

use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One entry as it stands, read without following it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Stat {
    pub(super) dev: u64,
    pub(super) ino: u64,
    mode: u32,
    pub(super) size: u64,
    pub(super) modified: SystemTime,
}

// The field types of `stat` differ between platforms, so the casts stay.
#[allow(clippy::unnecessary_cast)]
impl Stat {
    fn from_raw(raw: &libc::stat) -> Stat {
        let seconds = raw.st_mtime;
        let nanos = raw.st_mtime_nsec;
        let modified = if seconds >= 0 {
            UNIX_EPOCH + Duration::new(seconds as u64, nanos as u32)
        } else {
            UNIX_EPOCH
        };
        Stat {
            dev: raw.st_dev as u64,
            ino: raw.st_ino as u64,
            mode: raw.st_mode as u32,
            size: raw.st_size as u64,
            modified,
        }
    }

    fn kind(&self) -> u32 {
        self.mode & libc::S_IFMT as u32
    }

    pub(super) fn is_dir(&self) -> bool {
        self.kind() == libc::S_IFDIR as u32
    }

    pub(super) fn is_file(&self) -> bool {
        self.kind() == libc::S_IFREG as u32
    }

    pub(super) fn is_link(&self) -> bool {
        self.kind() == libc::S_IFLNK as u32
    }

    /// Whether both name one object.
    pub(super) fn same(&self, other: &Stat) -> bool {
        self.dev == other.dev && self.ino == other.ino
    }
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// Whether an error says a name was a link or not a directory where a
/// directory was required.
pub(super) fn not_a_real_dir(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ELOOP) | Some(libc::ENOTDIR)
    )
}

/// An open directory.
pub(super) struct Dir {
    fd: OwnedFd,
}

impl Dir {
    fn from_fd(fd: libc::c_int) -> Dir {
        // SAFETY: `fd` was just returned by a successful open and nothing
        // else owns it.
        Dir {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        }
    }

    /// Open the directory at `path`, following links on the way.
    pub(super) fn open(path: &Path) -> io::Result<Dir> {
        let path = c_name(path.as_os_str())?;
        // SAFETY: `path` is NUL-terminated and outlives the call.
        let fd = check(unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        })?;
        Ok(Dir::from_fd(fd))
    }

    fn open_at(&self, name: &OsStr, flags: libc::c_int, mode: libc::c_uint) -> io::Result<i32> {
        let name = c_name(name)?;
        // SAFETY: the descriptor is open and `name` is NUL-terminated.
        check(unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode,
            )
        })
    }

    /// Open the directory `name` in this one; a link or anything but a
    /// directory there refuses.
    pub(super) fn open_dir(&self, name: &OsStr) -> io::Result<Dir> {
        let fd = self.open_at(name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        Ok(Dir::from_fd(fd))
    }

    /// This directory as it stands.
    pub(super) fn stat(&self) -> io::Result<Stat> {
        let mut raw = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the descriptor is open and `raw` has room for a stat.
        check(unsafe { libc::fstat(self.fd.as_raw_fd(), raw.as_mut_ptr()) })?;
        // SAFETY: fstat succeeded, so it filled `raw`.
        Ok(Stat::from_raw(unsafe { &raw.assume_init() }))
    }

    /// The entry `name` in this directory, a link itself rather than what
    /// it leads to; `None` when there is none.
    pub(super) fn entry(&self, name: &OsStr) -> io::Result<Option<Stat>> {
        let c = c_name(name)?;
        let mut raw = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the descriptor is open, `c` is NUL-terminated and `raw`
        // has room for a stat.
        let result = unsafe {
            libc::fstatat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                raw.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        match check(result) {
            // SAFETY: fstatat succeeded, so it filled `raw`.
            Ok(_) => Ok(Some(Stat::from_raw(unsafe { &raw.assume_init() }))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The names this directory holds, without `.` and `..`.
    pub(super) fn names(&self) -> io::Result<Vec<OsString>> {
        // The descriptor's own entry under /dev/fd opens this directory,
        // whatever path once led to it.
        let mut names = Vec::new();
        for entry in std::fs::read_dir(format!("/dev/fd/{}", self.fd.as_raw_fd()))? {
            names.push(entry?.file_name());
        }
        names.sort();
        Ok(names)
    }

    /// Where the link `name` leads.
    pub(super) fn read_link(&self, name: &OsStr) -> io::Result<PathBuf> {
        let c = c_name(name)?;
        let mut buffer = vec![0u8; libc::PATH_MAX as usize];
        // SAFETY: the descriptor is open, `c` is NUL-terminated and the
        // buffer's length is passed.
        let length = unsafe {
            libc::readlinkat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if length < 0 {
            return Err(io::Error::last_os_error());
        }
        buffer.truncate(length as usize);
        Ok(PathBuf::from(
            <OsString as std::os::unix::ffi::OsStringExt>::from_vec(buffer),
        ))
    }

    /// Make the directory `name` in this one.
    pub(super) fn make_dir(&self, name: &OsStr) -> io::Result<()> {
        let c = c_name(name)?;
        // SAFETY: the descriptor is open and `c` is NUL-terminated.
        check(unsafe { libc::mkdirat(self.fd.as_raw_fd(), c.as_ptr(), 0o777) })?;
        Ok(())
    }

    /// Remove the directory `name` when it is empty.
    pub(super) fn remove_empty_dir(&self, name: &OsStr) -> io::Result<()> {
        let c = c_name(name)?;
        // SAFETY: the descriptor is open and `c` is NUL-terminated.
        check(unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), libc::AT_REMOVEDIR) })?;
        Ok(())
    }

    /// Create the file `name`, refusing when anything is there already.
    pub(super) fn create_new(&self, name: &OsStr) -> io::Result<File> {
        let fd = self.open_at(name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o666)?;
        // SAFETY: `fd` was just opened and nothing else owns it.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Open the existing file `name` to read and write; a link refuses.
    pub(super) fn open_file(&self, name: &OsStr, write: bool) -> io::Result<File> {
        let flags = if write { libc::O_RDWR } else { libc::O_RDONLY };
        let fd = self.open_at(name, flags, 0)?;
        // SAFETY: `fd` was just opened and nothing else owns it.
        let file = unsafe { File::from_raw_fd(fd) };
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        Ok(file)
    }
}

/// Rename `from` in `from_dir` to `to` in `to_dir`, refusing to replace
/// anything at `to`. A kernel or filesystem with no atomic rename that
/// refuses to replace refuses the move.
pub(super) fn rename_new(from_dir: &Dir, from: &OsStr, to_dir: &Dir, to: &OsStr) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let (from_c, to_c) = (c_name(from)?, c_name(to)?);
        // SAFETY: both descriptors are open and both names are
        // NUL-terminated and outlive the call.
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                from_dir.fd.as_raw_fd(),
                from_c.as_ptr(),
                to_dir.fd.as_raw_fd(),
                to_c.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::EINVAL) | Some(libc::ENOSYS)
        ) {
            return Err(error);
        }
    }
    let _ = (from_dir, from, to_dir, to);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this kernel or filesystem has no rename that refuses to replace, so arc moves nothing here",
    ))
}
