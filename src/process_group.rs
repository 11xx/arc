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

/// Write all of `bytes` to a child's input, so that a child which closed it
/// or exited makes the write fail as a broken pipe.
///
/// `main` restores SIGPIPE's default disposition so that arc ends quietly
/// when its own output's reader goes away. A write to a child's input raises
/// the same signal, which would end arc silently in the middle of a command.
/// The write runs with SIGPIPE blocked on the calling thread, and a SIGPIPE
/// it raised is taken from the thread before the mask is restored.
pub(crate) fn write_input(input: &mut impl std::io::Write, bytes: &[u8]) -> std::io::Result<()> {
    // SAFETY: the signal set is initialized by sigemptyset before use, and
    // pthread_sigmask and sigtimedwait only read and write the sets and
    // timespec passed to them.
    unsafe {
        let mut pipe = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
        libc::sigemptyset(pipe.as_mut_ptr());
        libc::sigaddset(pipe.as_mut_ptr(), libc::SIGPIPE);
        let pipe = pipe.assume_init();
        let mut previous = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
        let blocked = libc::pthread_sigmask(libc::SIG_BLOCK, &pipe, previous.as_mut_ptr());
        if blocked != 0 {
            return Err(std::io::Error::from_raw_os_error(blocked));
        }
        let written = input.write_all(bytes).and_then(|()| input.flush());
        let now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        while libc::sigtimedwait(&pipe, std::ptr::null_mut(), &now) == libc::SIGPIPE
            || std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
        {}
        libc::pthread_sigmask(libc::SIG_SETMASK, previous.as_ptr(), std::ptr::null_mut());
        written
    }
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
    _answers: crate::gitio::answers::WriteScope,
    child: Child,
    status: Option<ExitStatus>,
    report: Option<std::fs::File>,
    control: Option<std::fs::File>,
    reported: Option<ExitStatus>,
    report_bytes: Vec<u8>,
    pub(crate) observation: crate::verification_timing::CommandReport,
}

impl OwnedGroup {
    /// Spawn `command`, which must lead its own process group.
    pub(crate) fn spawn(command: &mut Command) -> std::io::Result<Self> {
        let _answers = crate::gitio::answers::WriteScope::project();
        Ok(Self::adopt(command.counted_spawn()?))
    }

    /// Hold `child`, spawned as the leader of its own process group and not
    /// yet waited for.
    pub(crate) fn adopt(child: Child) -> Self {
        Self {
            _answers: crate::gitio::answers::WriteScope::project(),
            child,
            status: None,
            report: None,
            control: None,
            reported: None,
            report_bytes: Vec::new(),
            observation: Default::default(),
        }
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

    /// Let the holder stop the command while retaining locks for detached children.
    pub(crate) fn stop_through(&mut self, pipe: std::os::fd::OwnedFd) {
        self.control = Some(std::fs::File::from(pipe));
    }

    /// Whether the command has exited: the leader's report when it makes
    /// one, else the leader's own exit. Its PID stays reserved either way.
    pub(crate) fn leader_exited(&mut self) -> std::io::Result<bool> {
        if let Some(report) = self.report.as_mut() {
            use std::os::unix::process::ExitStatusExt;
            let mut bytes = [0_u8; 1024];
            loop {
                match report.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(read) => self.report_bytes.extend_from_slice(&bytes[..read]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
                while let Some(end) = self.report_bytes.iter().position(|byte| *byte == b'\n') {
                    self.observation = serde_json::from_slice(&self.report_bytes[..end])?;
                    self.report_bytes.drain(..=end);
                    if let Some(raw) = self.observation.status {
                        self.reported = Some(ExitStatus::from_raw(raw));
                    }
                }
                if self.report_bytes.len() > 4096 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "command report exceeds 4096 bytes",
                    ));
                }
            }
        }
        if self.reported.is_some() {
            return Ok(true);
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

    /// Kill whatever remains in the group and leave the leader unreaped, so
    /// its group ID stays reserved until [`OwnedGroup::stop`].
    pub(crate) fn kill(&mut self) -> std::io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        if let Some(control) = self.control.as_mut() {
            let _ = write_input(control, b"x");
            return Ok(());
        }
        kill_process_group(self.child.id())
    }

    /// Kill whatever remains in the group, then reap the leader and return
    /// the command's exit status: the leader's report when it made one, the
    /// leader's own status when it had exited, SIGKILL otherwise.
    pub(crate) fn stop(&mut self) -> std::io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        if self.report.is_some() {
            self.leader_exited()?;
        }
        if let Some(mut control) = self.control.take() {
            use std::os::fd::AsRawFd;
            use std::os::unix::process::ExitStatusExt;
            let _ = write_input(&mut control, b"x");
            drop(control);
            let stopped: std::io::Result<()> = (|| {
                while !self.observation.stopped {
                    let pipe = self
                        .report
                        .as_ref()
                        .expect("controlled holder reports its exit");
                    crate::exclusive::wait_readable(pipe.as_raw_fd(), None)?;
                    self.leader_exited()?;
                    if self.leader_finished()? {
                        self.leader_exited()?;
                        break;
                    }
                }
                Ok(())
            })();
            let status = self.reported.unwrap_or(ExitStatus::from_raw(libc::SIGKILL));
            self.status = Some(status);
            let pid = self.child.id() as i32;
            std::thread::spawn(move || {
                // SAFETY: this unreaped child belongs to the holder guard.
                while unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) } < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                }
            });
            stopped?;
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

    /// How long a unit test waits for a fixture process or thread to reach a
    /// step it is certain to reach before calling the test hung. It bounds failures,
    /// never a passing run, so it is as long as the CLI suite's `HANG_BOUND`.
    pub(crate) const HANG_BOUND: Duration = Duration::from_secs(120);

    /// The seconds of a fixture child that a deadline must cut short. It
    /// outlasts [`HANG_BOUND`], so code that waits the child out instead of
    /// killing it at its deadline fails a test that bounds its run by
    /// [`HANG_BOUND`].
    pub(crate) const OUTLIVING_SECS: u64 = HANG_BOUND.as_secs() * 2;

    /// The PID a fixture wrote to `name` under `dir`, waiting for it to appear.
    pub(crate) fn read_pid(dir: &Path, name: &str) -> u32 {
        let path = dir.join(name);
        let deadline = Instant::now() + HANG_BOUND;
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

    /// Run `body` in a copy of the test binary with SIGPIPE at its default
    /// disposition, as `main` leaves it, and fail if that copy fails or ends
    /// by a signal. `test` is the calling test's full name, which the copy
    /// runs alone. The test harness ignores SIGPIPE, so a write that would
    /// end arc passes unnoticed in it.
    pub(crate) fn under_default_sigpipe(test: &str, body: impl FnOnce()) {
        const CHILD: &str = "ARC_TEST_DEFAULT_SIGPIPE";
        if std::env::var(CHILD).as_deref() == Ok(test) {
            // SAFETY: restoring a default disposition takes no handler.
            unsafe {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            }
            body();
            return;
        }
        let copy = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture", "--test-threads", "1"])
            .env(CHILD, test)
            .output()
            .unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert!(
            copy.status.success(),
            "{test} under default SIGPIPE ended with {:?} (signal {:?}): {}{}",
            copy.status.code(),
            copy.status.signal(),
            String::from_utf8_lossy(&copy.stdout),
            String::from_utf8_lossy(&copy.stderr)
        );
        assert!(
            String::from_utf8_lossy(&copy.stdout).contains("1 passed"),
            "{test} did not run in its copy: {}",
            String::from_utf8_lossy(&copy.stdout)
        );
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

    /// The `sh -c` script of the fixture [`LeaderObservation`] watches, run
    /// in its directory. The leader writes `leader.pid`, starts a descendant
    /// that escapes into its own session, writes `escaped.pid` and holds the
    /// leader's output open until the test kills it, and exits only once that
    /// descendant has written its PID. A kill of the leader's group that
    /// follows its exit cannot catch the descendant before it escapes.
    pub(crate) fn leader_script() -> String {
        format!(
            "echo $$ > leader.pid; \
             setsid sh -c 'echo $$ > escaped.pid; exec sleep {}' & \
             while ! test -s escaped.pid; do sleep 0.01; done",
            OUTLIVING_SECS
        )
    }

    /// How long after the leader exits [`LeaderObservation`] checks that it
    /// is still unreaped. Code that reaps on exit does so within its next
    /// poll; code that holds the leader keeps it until the test releases the
    /// escaped descendant, so this sets how promptly a reaping variant is
    /// caught and never fails a holding one.
    const REAP_GRACE: Duration = Duration::from_millis(200);

    /// A [`leader_script`] fixture whose leader exits while its escaped
    /// descendant keeps the leader's output pipe open.
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
                ends_within(self.leader, HANG_BOUND),
                "fixture leader did not exit"
            );
            std::thread::sleep(REAP_GRACE);
            state(self.leader) == Some('Z')
        }

        /// Kill the escaped descendant, which releases the leader's output
        /// pipe so the code under test can finish.
        pub(crate) fn kill_escaped(&self) {
            kill(read_pid(&self.dir, "escaped.pid"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    /// A child that has closed its input fails arc's write to it as a broken
    /// pipe. Rejects a write that raises SIGPIPE, which under the disposition
    /// `main` restores ends arc by the signal with nothing printed.
    #[test]
    fn a_write_to_a_closed_child_input_is_a_broken_pipe_not_a_signal() {
        test_support::under_default_sigpipe(
            "process_group::tests::a_write_to_a_closed_child_input_is_a_broken_pipe_not_a_signal",
            || {
                let mut child = Command::new("true").stdin(Stdio::piped()).spawn().unwrap();
                let mut input = child.stdin.take().unwrap();
                child.wait().unwrap();
                let error = write_input(&mut input, b"input\n").unwrap_err();
                assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
            },
        );
    }
}
