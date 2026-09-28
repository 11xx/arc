//! Bounds for a child process arc runs on a project's behalf.
//!
//! A child spawned as the leader of its own process group can be killed
//! together with everything it started, and its diagnostic output is kept as
//! a bounded tail so a child that prints without bound cannot exhaust memory.

use std::io::Read;

/// Bound on the stderr a child's failure report carries.
pub const STDERR_TAIL_BYTES: usize = 4096;

const SIGKILL: i32 = 9;
const ESRCH: i32 = 3;

extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

/// Kill the process group `pid` leads.
///
/// A group that exited between the caller's completion poll and kill(2) is
/// not a failure to kill it.
pub fn kill_process_group(pid: u32) -> std::io::Result<()> {
    let pid = i32::try_from(pid).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "child pid exceeds i32")
    })?;
    // SAFETY: `kill` is called with a negated child PID created as the leader
    // of its own process group; SIGKILL requires no borrowed memory contract.
    if unsafe { kill(-pid, SIGKILL) } == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

/// The final `limit` bytes of a reader, for a bounded diagnostic.
pub fn read_tail(mut reader: impl Read, limit: usize) -> std::io::Result<String> {
    let mut tail = Vec::with_capacity(limit);
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            return Ok(String::from_utf8_lossy(&tail).into_owned());
        }
        if read >= limit {
            tail.clear();
            tail.extend_from_slice(&chunk[read - limit..read]);
            continue;
        }
        let overflow = tail.len().saturating_add(read).saturating_sub(limit);
        if overflow > 0 {
            tail.drain(..overflow);
        }
        tail.extend_from_slice(&chunk[..read]);
    }
}
