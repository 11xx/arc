//! Write confinement for a test harness and every process it starts, enforced
//! by the kernel through Landlock.
//!
//! Every file system access right the running kernel's Landlock offers is
//! handled. Reading, listing and executing are granted everywhere; everything
//! else, writing, truncating, creating, removing, renaming, linking, device
//! ioctls and connecting to pathname sockets created outside the domain, only
//! beneath the roots given. The device nodes a process opens for writing get
//! exactly what it needs of them. A Landlock domain is inherited by every
//! child and can never be lifted, so whatever path a test, a helper or the
//! program under test names, nothing outside the roots is written.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const CREATE_RULESET_VERSION: u32 = 1;
const RULE_PATH_BENEATH: libc::c_int = 1;

pub(crate) const EXECUTE: u64 = 1 << 0;
pub(crate) const WRITE_FILE: u64 = 1 << 1;
pub(crate) const READ_FILE: u64 = 1 << 2;
pub(crate) const READ_DIR: u64 = 1 << 3;
/// The rights of ABI 1, from executing through making symbolic links.
const ABI_1: u64 = (1 << 13) - 1;
/// Linking or renaming into another directory; ABI 2.
const REFER: u64 = 1 << 13;
/// Truncating by path or open; ABI 3.
pub(crate) const TRUNCATE: u64 = 1 << 14;
/// Device ioctls; ABI 5.
pub(crate) const IOCTL_DEV: u64 = 1 << 15;
/// Connecting to a pathname socket created outside the domain; ABI 9.
const RESOLVE_UNIX: u64 = 1 << 16;

/// Below ABI 3 a path could still be truncated, and below ABI 2 every
/// cross-directory rename is refused, so those kernels do not confine.
const MINIMUM_ABI: i64 = 3;

/// What is granted everywhere: reading never writes.
const READS: u64 = EXECUTE | READ_FILE | READ_DIR;

/// The device nodes a process opens for writing, and what it needs of each:
/// output to the null device, and output and terminal control to the
/// terminal.
const DEVICES: [(&str, u64); 3] = [
    ("/dev/null", WRITE_FILE | TRUNCATE),
    ("/dev/tty", WRITE_FILE | TRUNCATE | IOCTL_DEV),
    ("/dev/pts", WRITE_FILE | TRUNCATE | IOCTL_DEV),
];

/// Every file system right the Landlock ABI `abi` offers.
pub(crate) fn rights_of(abi: i64) -> u64 {
    let mut rights = ABI_1;
    if abi >= 2 {
        rights |= REFER;
    }
    if abi >= 3 {
        rights |= TRUNCATE;
    }
    if abi >= 5 {
        rights |= IOCTL_DEV;
    }
    if abi >= 9 {
        rights |= RESOLVE_UNIX;
    }
    rights
}

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// Confine this process, and every process it will start, to writing beneath
/// `roots`. The error says why the kernel cannot, and leaves the process
/// unconfined.
pub(crate) fn confine(roots: &[&Path]) -> Result<(), String> {
    // SAFETY: a null attribute with size 0 and the version flag only queries
    // the ABI the kernel supports.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            CREATE_RULESET_VERSION,
        )
    };
    if abi < 0 {
        return Err(format!(
            "this kernel has no Landlock: {}",
            std::io::Error::last_os_error()
        ));
    }
    if abi < MINIMUM_ABI {
        return Err(format!(
            "this kernel's Landlock ABI {abi} cannot refuse truncation (ABI {MINIMUM_ABI})"
        ));
    }
    let handled = rights_of(abi);
    let attr = RulesetAttr {
        handled_access_fs: handled,
    };
    // SAFETY: attr is a valid ruleset attribute of the size passed.
    let ruleset = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0u32,
        )
    };
    if ruleset < 0 {
        return Err(format!(
            "Landlock refused a ruleset: {}",
            std::io::Error::last_os_error()
        ));
    }
    let ruleset = ruleset as libc::c_int;
    let result = (|| {
        allow(ruleset, Path::new("/"), READS)?;
        for root in roots {
            allow(ruleset, root, handled)?;
        }
        for (device, access) in DEVICES {
            // A node this host lacks needs no rule.
            let _ = allow(ruleset, Path::new(device), access & handled);
        }
        // SAFETY: prctl with PR_SET_NO_NEW_PRIVS takes integer arguments only.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(format!("no_new_privs: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: ruleset is a Landlock ruleset descriptor owned here.
        if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0u32) } != 0 {
            return Err(format!(
                "Landlock refused the restriction: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    })();
    // SAFETY: ruleset is a descriptor this function opened and owns.
    unsafe { libc::close(ruleset) };
    result
}

fn allow(ruleset: libc::c_int, path: &Path, access: u64) -> Result<(), String> {
    let name = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("{} holds a NUL byte", path.display()))?;
    // SAFETY: name is a NUL-terminated path; O_PATH opens without reading.
    let fd = unsafe { libc::open(name.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(format!(
            "{}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let rule = PathBeneathAttr {
        allowed_access: access,
        parent_fd: fd,
    };
    // SAFETY: rule is a valid path-beneath attribute naming the open fd.
    let added = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset,
            RULE_PATH_BENEATH,
            &rule as *const PathBeneathAttr,
            0u32,
        )
    };
    let error = std::io::Error::last_os_error();
    // SAFETY: fd was opened above and is not used again.
    unsafe { libc::close(fd) };
    if added != 0 {
        return Err(format!("{}: {error}", path.display()));
    }
    Ok(())
}
