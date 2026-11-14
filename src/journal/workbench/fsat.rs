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

/// The directories that reach an open descriptor by path: `listing` reads
/// an open directory's entries, `naming` says where it is now.
struct DescriptorDirs {
    listing: PathBuf,
    naming: PathBuf,
}

impl DescriptorDirs {
    fn get() -> DescriptorDirs {
        let root = PathBuf::from("/");
        // A debug build lets a test move both under a root where they are
        // absent.
        #[cfg(debug_assertions)]
        let root = std::env::var_os("ARC_TEST_WORKBENCH_DESCRIPTOR_ROOT")
            .map(PathBuf::from)
            .unwrap_or(root);
        DescriptorDirs {
            listing: root.join("dev/fd"),
            naming: root.join("proc/self/fd"),
        }
    }
}

/// The error for a descriptor directory that is not there, or `None` when
/// it is.
fn unavailable(dir: &Path) -> Option<io::Error> {
    (!dir.is_dir()).then(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} is not available here, and arc reads and names the workbench directories it opens through /dev/fd and /proc/self/fd",
                dir.display()
            ),
        )
    })
}

/// Refuse, before anything is read or moved, where either descriptor
/// directory is missing: a walk would fail partway, and a move could not
/// say where an escaped directory went.
pub(super) fn require_descriptor_dirs() -> io::Result<()> {
    let dirs = DescriptorDirs::get();
    match unavailable(&dirs.listing).or_else(|| unavailable(&dirs.naming)) {
        Some(error) => Err(error),
        None => Ok(()),
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

    /// The names this directory holds, without `.` and `..`, read as a
    /// stream in the order the filesystem gives them, so a caller that stops
    /// early never reads the rest.
    pub(super) fn names(&self) -> io::Result<impl Iterator<Item = io::Result<OsString>>> {
        // The descriptor's own entry under /dev/fd opens this directory,
        // whatever path once led to it.
        let listing = DescriptorDirs::get().listing;
        let entries =
            std::fs::read_dir(listing.join(self.fd.as_raw_fd().to_string())).map_err(|error| {
                match unavailable(&listing) {
                    Some(missing) => missing,
                    None => error,
                }
            })?;
        Ok(entries.map(|entry| entry.map(|entry| entry.file_name())))
    }

    /// Where this directory is now, as far as the kernel can name it.
    pub(super) fn current_path(&self) -> Option<PathBuf> {
        let naming = DescriptorDirs::get().naming;
        std::fs::read_link(naming.join(self.fd.as_raw_fd().to_string())).ok()
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

    /// Remove the file `name`.
    pub(super) fn remove_file(&self, name: &OsStr) -> io::Result<()> {
        let c = c_name(name)?;
        // SAFETY: the descriptor is open and `c` is NUL-terminated.
        check(unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), 0) })?;
        Ok(())
    }

    /// Rename `from` over `to`, both in this directory, replacing what
    /// stands at `to` in one step: a reader opening `to` finds the old file
    /// or the new one, never part of either.
    pub(super) fn rename_over(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        let (from, to) = (c_name(from)?, c_name(to)?);
        let fd = self.fd.as_raw_fd();
        // SAFETY: the descriptor is open and both names are NUL-terminated.
        check(unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) })?;
        Ok(())
    }

    /// Create the file `name` with at most the permission bits `mode`,
    /// refusing when anything is there already. Neither the umask nor the
    /// directory's default ACL grants more than `mode`.
    pub(super) fn create_new(&self, name: &OsStr, mode: libc::c_uint) -> io::Result<File> {
        let fd = self.open_at(name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, mode)?;
        // SAFETY: `fd` was just opened and nothing else owns it.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Open the existing regular file `name` to read. A link, a pipe, a
    /// device or anything else that is not a regular file refuses, and the
    /// open never waits on one: it neither follows a link nor blocks, and
    /// the file's kind is read from the descriptor it opened.
    pub(super) fn open_file(&self, name: &OsStr) -> io::Result<File> {
        let fd = self.open_at(name, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        // SAFETY: `fd` was just opened and nothing else owns it.
        regular(unsafe { File::from_raw_fd(fd) })
    }
}

/// Open the regular file at `path` to read, as [`Dir::open_file`] does:
/// without following a link in its last component or waiting on a pipe.
pub(super) fn open_regular(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    regular(file)
}

/// Give `to` exactly the access of `from`: its permission bits, and its
/// access ACL when it carries one, or none when it does not, whatever `to`
/// inherited from its directory's default ACL. Without the ACL, the group
/// bits of a file with one hold the ACL's mask, so its mode alone would
/// grant the owning group what the mask allows.
pub(super) fn match_access(from: &File, to: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match access_acl(from)? {
        Some(acl) => set_access_acl(to, &acl)?,
        None => remove_access_acl(to)?,
    }
    to.set_permissions(std::fs::Permissions::from_mode(
        from.metadata()?.permissions().mode() & 0o7777,
    ))
}

#[cfg(target_os = "linux")]
const ACCESS_ACL: &[u8] = b"system.posix_acl_access\0";

/// Whether an error says a file carries no ACL, or its filesystem none.
#[cfg(target_os = "linux")]
fn no_acl(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENODATA) | Some(libc::ENOTSUP)
    )
}

/// The access ACL `file` carries, in the kernel's form, or `None`.
fn access_acl(file: &File) -> io::Result<Option<Vec<u8>>> {
    #[cfg(target_os = "linux")]
    loop {
        // SAFETY: the descriptor is open and the name NUL-terminated; a null
        // buffer of length 0 asks for the size.
        let size = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                ACCESS_ACL.as_ptr().cast(),
                std::ptr::null_mut(),
                0,
            )
        };
        if size < 0 {
            let error = io::Error::last_os_error();
            return if no_acl(&error) { Ok(None) } else { Err(error) };
        }
        let mut buffer = vec![0u8; size as usize];
        // SAFETY: as above, with the buffer's length passed.
        let length = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                ACCESS_ACL.as_ptr().cast(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if length >= 0 {
            buffer.truncate(length as usize);
            return Ok(Some(buffer));
        }
        let error = io::Error::last_os_error();
        // The ACL grew between the two calls: ask again.
        if error.raw_os_error() != Some(libc::ERANGE) {
            return if no_acl(&error) { Ok(None) } else { Err(error) };
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Ok(None)
    }
}

fn set_access_acl(file: &File, acl: &[u8]) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    // SAFETY: the descriptor is open, the name NUL-terminated, and the
    // value's length passed.
    check(unsafe {
        libc::fsetxattr(
            file.as_raw_fd(),
            ACCESS_ACL.as_ptr().cast(),
            acl.as_ptr().cast(),
            acl.len(),
            0,
        )
    })?;
    let _ = (file, acl);
    Ok(())
}

fn remove_access_acl(file: &File) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: the descriptor is open and the name NUL-terminated.
        let result = unsafe { libc::fremovexattr(file.as_raw_fd(), ACCESS_ACL.as_ptr().cast()) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if !no_acl(&error) {
                return Err(error);
            }
        }
    }
    let _ = file;
    Ok(())
}

/// `file`, when the descriptor is a regular file.
fn regular(file: File) -> io::Result<File> {
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

/// Rename `from` in `from_dir` to `to` in `to_dir`, refusing to replace
/// anything at `to`. A kernel or filesystem with no atomic rename that
/// refuses to replace refuses the move.
pub(super) fn rename_new(from_dir: &Dir, from: &OsStr, to_dir: &Dir, to: &OsStr) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let (from_c, to_c) = (c_name(from)?, c_name(to)?);
        let error = if no_replace_unsupported() {
            // What a filesystem without the flag answers.
            io::Error::from_raw_os_error(libc::EINVAL)
        } else {
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
            io::Error::last_os_error()
        };
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

/// Whether a debug build is told to answer every no-replace rename as a
/// filesystem without one does, so a test reaches the refusal.
#[cfg(target_os = "linux")]
fn no_replace_unsupported() -> bool {
    #[cfg(debug_assertions)]
    if std::env::var_os("ARC_TEST_WORKBENCH_NO_RENAME_NOREPLACE").is_some() {
        return true;
    }
    false
}
