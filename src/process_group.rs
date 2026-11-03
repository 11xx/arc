//! Bounds for a child process arc runs on a project's behalf.
//!
//! A child spawned as the leader of its own process group can be killed
//! together with everything it started, and its diagnostic output is kept as
//! a bounded tail so a child that prints without bound cannot exhaust memory.
//! [`OwnedGroup`] keeps the group's identity reserved until it is stopped.

use crate::perf::Counted;
use std::io::Read;
use std::process::{Child, Command, ExitStatus};

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

/// A child that leads its own process group, held until that group is
/// stopped.
///
/// The leader's exit is observed without reaping it, and it is reaped only
/// after its group is killed. The unreaped leader keeps its PID, and so the
/// group ID, reserved for every signal arc sends: once a reaped leader's
/// remaining members leave the group, the kernel may give that number to an
/// unrelated process, and a kill sent to it would reach a group arc never
/// started. Dropping the guard stops the group.
///
/// A leader that holds the group's resources on a command's behalf (a slot's
/// use holder) reports the command's exit through a pipe and keeps running
/// until its own descendants end. Its report counts as the leader's exit,
/// and its status as the group's.
pub(crate) struct OwnedGroup {
    child: Child,
    status: Option<ExitStatus>,
    report: Option<std::fs::File>,
    reported: Option<ExitStatus>,
}

impl OwnedGroup {
    /// Spawn `command`, which must lead its own process group.
    pub(crate) fn spawn(command: &mut Command) -> std::io::Result<Self> {
        Ok(Self {
            child: command.counted_spawn()?,
            status: None,
            report: None,
            reported: None,
        })
    }

    /// Take the command's exit from the leader's report on `pipe`.
    pub(crate) fn report_through(&mut self, pipe: std::os::fd::OwnedFd) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        // SAFETY: fcntl reads and sets flags on a descriptor `pipe` owns.
        unsafe {
            let flags = libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL);
            if flags < 0
                || libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
        self.report = Some(std::fs::File::from(pipe));
        Ok(())
    }

    pub(crate) fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Whether the command has exited: the leader's report when it makes
    /// one, else the leader's own exit. Its PID stays reserved either way.
    pub(crate) fn leader_exited(&mut self) -> std::io::Result<bool> {
        if self.reported.is_some() {
            return Ok(true);
        }
        if let Some(report) = self.report.as_mut() {
            use std::os::unix::process::ExitStatusExt;
            let mut bytes = [0_u8; 4];
            match report.read(&mut bytes) {
                Ok(4) => {
                    self.reported = Some(ExitStatus::from_raw(i32::from_le_bytes(bytes)));
                    return Ok(true);
                }
                Ok(_) => self.report = None,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        self.leader_finished()
    }

    fn leader_finished(&self) -> std::io::Result<bool> {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: waitid initializes `info`; WNOWAIT leaves the leader
        // unreaped, so its PID stays reserved until `stop`.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: waitid succeeded and initialized the zeroed structure; with
        // WNOHANG a zero `si_pid` means no child changed state.
        Ok(unsafe { info.assume_init().si_pid() } != 0)
    }

    /// Kill whatever remains in the group, then reap the leader and return
    /// the command's exit status: the leader's report when it made one, the
    /// leader's own status when it had exited, SIGKILL otherwise.
    pub(crate) fn stop(&mut self) -> std::io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        kill_process_group(self.child.id())?;
        let status = self.child.wait()?;
        let status = self.reported.unwrap_or(status);
        self.status = Some(status);
        Ok(status)
    }
}

impl Drop for OwnedGroup {
    fn drop(&mut self) {
        let _ = self.stop();
    }
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

/// Join a pipe worker within the process deadline. An unfinished worker owns
/// its buffers and pipe; dropping its handle detaches it without extending
/// the deadline or borrowing caller state.
pub(crate) fn join_before_deadline<T>(
    worker: std::thread::JoinHandle<T>,
    deadline: Option<std::time::Instant>,
    name: &str,
) -> anyhow::Result<Option<T>> {
    if let Some(deadline) = deadline {
        while !worker.is_finished() {
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(std::time::Duration::from_millis(1)),
            );
        }
    }
    worker
        .join()
        .map(Some)
        .map_err(|_| anyhow::anyhow!("{name} panicked"))
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Observations of fixture processes by PID, for tests about which
    //! processes outlive the group arc owns.

    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    /// The PID a fixture wrote to `name` under `dir`, waiting for it to appear.
    pub(crate) fn read_pid(dir: &Path, name: &str) -> u32 {
        let path = dir.join(name);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(pid) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "{} was never written",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The process state letter `/proc` reports for `pid`, if it exists.
    pub(crate) fn state(pid: u32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(") ")?.1.chars().next()
    }

    /// Whether `pid` has ended (gone, or a zombie no one has reaped yet)
    /// within `bound`.
    pub(crate) fn ends_within(pid: u32, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        loop {
            if matches!(state(pid), None | Some('Z' | 'X')) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Kill a leftover fixture process group so a failing test leaves
    /// nothing running.
    pub(crate) fn kill(pid: u32) {
        let pid = i32::try_from(pid).unwrap();
        // SAFETY: kill(2) takes no memory; the fixture wrote this PID.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
            libc::kill(pid, libc::SIGKILL);
        }
    }

    /// A fixture whose leader writes `leader.pid` and exits while a
    /// descendant that escaped into its own session, `escaped.pid`, keeps the
    /// leader's output pipe open.
    pub(crate) struct LeaderObservation {
        dir: PathBuf,
        leader: u32,
    }

    impl LeaderObservation {
        pub(crate) fn new(dir: &Path) -> Self {
            Self {
                dir: dir.to_owned(),
                leader: read_pid(dir, "leader.pid"),
            }
        }

        /// Whether the exited leader's PID is still held as an unreaped
        /// zombie a moment after it exits, which is what keeps its group ID
        /// from being reused.
        pub(crate) fn reserved_after_exit(&self) -> bool {
            assert!(
                ends_within(self.leader, Duration::from_secs(2)),
                "fixture leader did not exit"
            );
            std::thread::sleep(Duration::from_millis(200));
            state(self.leader) == Some('Z')
        }

        pub(crate) fn kill_escaped(&self) {
            kill(read_pid(&self.dir, "escaped.pid"));
        }
    }
}
