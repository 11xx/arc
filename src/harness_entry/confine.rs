//! Write confinement for a test harness and every process it starts, enforced
//! by the kernel through Landlock.
//!
//! Writes, creations, removals, renames, links and truncations are allowed
//! only beneath the roots given and on the device nodes a process opens for
//! writing; reading and executing stay unrestricted. A Landlock domain is
//! inherited by every child and can never be lifted, so whatever path a test,
//! a helper or the program under test names, nothing outside the roots is
//! written.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const CREATE_RULESET_VERSION: u32 = 1;
const RULE_PATH_BENEATH: libc::c_int = 1;

const WRITE_FILE: u64 = 1 << 1;
const REMOVE_DIR: u64 = 1 << 4;
const REMOVE_FILE: u64 = 1 << 5;
const MAKE_CHAR: u64 = 1 << 6;
const MAKE_DIR: u64 = 1 << 7;
const MAKE_REG: u64 = 1 << 8;
const MAKE_SOCK: u64 = 1 << 9;
const MAKE_FIFO: u64 = 1 << 10;
const MAKE_BLOCK: u64 = 1 << 11;
const MAKE_SYM: u64 = 1 << 12;
/// Linking or renaming into another directory; ABI 2.
const REFER: u64 = 1 << 13;
/// Truncating by path or open; ABI 3.
const TRUNCATE: u64 = 1 << 14;

/// Every right that changes the file system. Below ABI 3 a path could still
/// be truncated, and below ABI 2 every cross-directory rename is refused, so
/// those kernels do not confine.
const WRITES: u64 = WRITE_FILE
    | REMOVE_DIR
    | REMOVE_FILE
    | MAKE_CHAR
    | MAKE_DIR
    | MAKE_REG
    | MAKE_SOCK
    | MAKE_FIFO
    | MAKE_BLOCK
    | MAKE_SYM
    | REFER
    | TRUNCATE;
const MINIMUM_ABI: i64 = 3;

/// The rights a device node may be granted: it is a file, not a directory.
const DEVICE_WRITES: u64 = WRITE_FILE | TRUNCATE;

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
/// `roots` and to the `devices` that exist. The error says why the kernel
/// cannot, and leaves the process unconfined.
pub(crate) fn confine(roots: &[&Path], devices: &[&Path]) -> Result<(), String> {
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
    let attr = RulesetAttr {
        handled_access_fs: WRITES,
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
        for root in roots {
            allow(ruleset, root, WRITES)?;
        }
        for device in devices {
            // A node this host lacks needs no rule.
            let _ = allow(ruleset, device, DEVICE_WRITES);
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
