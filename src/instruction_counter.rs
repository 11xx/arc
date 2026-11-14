//! A counter installed before exec, inherited by every thread and descendant.

#[cfg(target_os = "linux")]
mod linux {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    #[repr(C)]
    struct Attr {
        kind: u32,
        size: u32,
        config: u64,
        period: u64,
        sample_type: u64,
        read_format: u64,
        flags: u64,
        wakeup: u32,
        bp_type: u32,
        config1: u64,
    }

    /// The child waits before exec while a holder thread installs its counter.
    pub struct Pending(std::thread::JoinHandle<Option<Counter>>);
    pub struct Counter {
        instructions: OwnedFd,
        cpu: Option<OwnedFd>,
    }

    pub fn prepare(command: &mut Command) -> Option<Pending> {
        use std::io::{Read, Write};
        let (mut parent, child) = std::os::unix::net::UnixStream::pair().ok()?;
        // A host that refuses the thread has no counter; the command runs unmeasured.
        let worker = std::thread::Builder::new()
            .spawn(move || {
                let mut pid = [0_u8; 4];
                parent.read_exact(&mut pid).ok()?;
                let pid = i32::from_ne_bytes(pid);
                let counter = open(pid, 0, 1).map(|instructions| Counter {
                    instructions,
                    cpu: open(pid, 1, 1),
                });
                parent.write_all(&[0]).ok()?;
                counter
            })
            .ok()?;
        // SAFETY: the child uses stack storage and async-signal-safe syscalls only.
        unsafe {
            command.pre_exec(move || {
                let pid = libc::getpid().to_ne_bytes();
                if libc::write(child.as_raw_fd(), pid.as_ptr().cast(), pid.len())
                    != pid.len() as isize
                {
                    return Err(std::io::Error::last_os_error());
                }
                let mut byte = 0_u8;
                loop {
                    match libc::read(child.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) {
                        1 => return Ok(()),
                        -1 if std::io::Error::last_os_error().kind()
                            == std::io::ErrorKind::Interrupted =>
                        {
                            continue
                        }
                        _ => return Err(std::io::Error::from_raw_os_error(libc::EPIPE)),
                    }
                }
            });
        }
        Some(Pending(worker))
    }

    impl Pending {
        pub fn receive(self) -> Option<Counter> {
            self.0.join().ok().flatten()
        }
    }

    fn open(pid: i32, kind: u32, config: u64) -> Option<OwnedFd> {
        let attr = Attr {
            kind,
            size: std::mem::size_of::<Attr>() as u32,
            config,
            period: 0,
            sample_type: 0,
            read_format: 3,
            flags: 1 | (1 << 1) | (1 << 5) | (1 << 6) | (1 << 12),
            wakeup: 0,
            bp_type: 0,
            config1: 0,
        };
        // SAFETY: attr names the waiting child; only the holder owns the descriptor.
        let fd = unsafe { libc::syscall(libc::SYS_perf_event_open, &attr, pid, -1, -1, 8) as i32 };
        (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
    }
    fn read(fd: &OwnedFd) -> Option<u64> {
        let mut values = [0_u64; 3];
        // SAFETY: read initializes at most the size of this array.
        let bytes = unsafe {
            libc::read(
                fd.as_raw_fd(),
                values.as_mut_ptr().cast(),
                std::mem::size_of_val(&values),
            )
        };
        (bytes == std::mem::size_of_val(&values) as isize
            && values[1] > 0
            && values[1] == values[2])
            .then_some(values[0])
    }
    impl Counter {
        /// The inherited task clock includes CPU that auto-reaping hides from wait4.
        pub fn cpu_ms(&self) -> Option<u64> {
            Some(read(self.cpu.as_ref()?)? / 1_000_000)
        }
        /// A multiplexed or incomplete counter is absent, never a smaller workload.
        pub fn finish(self) -> Option<u64> {
            read(&self.instructions)
        }
    }
}
#[cfg(target_os = "linux")]
pub use linux::prepare;

#[cfg(not(target_os = "linux"))]
pub fn prepare(_: &mut std::process::Command) -> Option<Pending> {
    None
}
#[cfg(not(target_os = "linux"))]
pub struct Pending;
#[cfg(not(target_os = "linux"))]
impl Pending {
    pub fn receive(self) -> Option<Self> {
        None
    }
    pub fn cpu_ms(&self) -> Option<u64> {
        None
    }
    pub fn finish(self) -> Option<u64> {
        None
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    /// A permitted counter observes waited descendants, including work after fork.
    /// Rejects counting only the shell leader; unsupported counters remain absent.
    #[test]
    fn inherited_instructions_include_the_commands_descendants() {
        fn count(script: &str) -> Option<u64> {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", script]);
            let pending = super::prepare(&mut command)?;
            let mut child = command.spawn().unwrap();
            let counter = pending.receive();
            assert!(child.wait().unwrap().success());
            counter?.finish()
        }
        let Some(shell) = count("true") else {
            return;
        };
        let Some(tree) = count("python3 -c 'sum(range(2000000))'; :") else {
            return;
        };
        assert!(
            tree > shell * 10,
            "descendant instructions missing: shell {shell}, tree {tree}"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod ownership_tests {
    /// A gate cannot disable a holder-owned inherited counter with prctl.
    /// Rejects a near-zero count after the gate disables its own perf events.
    #[test]
    fn the_gate_cannot_disable_the_holders_counter() {
        fn count(disable: bool) -> Option<u64> {
            let mut command = std::process::Command::new("python3");
            command.args(["-c", &format!("import ctypes, subprocess, sys; ctypes.CDLL(None).prctl(31,0,0,0,0) if {disable} else None; subprocess.run([sys.executable, '-c', 'sum(range(2000000))'], check=True)", disable=if disable {"True"} else {"False"})]);
            let pending = super::prepare(&mut command)?;
            let mut child = command.spawn().unwrap();
            let counter = pending.receive();
            assert!(child.wait().unwrap().success());
            counter?.finish()
        }
        let Some(normal) = count(false) else { return };
        let disabled = count(true).expect("counter remains available");
        assert!(
            disabled > normal / 2,
            "gate disabled counter: normal {normal}, disabled {disabled}"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod cpu_tests {
    /// Tree CPU includes auto-reaped children and kernel CPU rather than charging them to others.
    /// Rejects waited rusage, a leader-only task clock and a clock filtered to user CPU.
    #[test]
    #[expect(
        clippy::zombie_processes,
        reason = "wait4 reaps the child and returns its CPU usage"
    )]
    fn inherited_task_cpu_includes_auto_reaped_children() {
        let script = "import os,signal,time\nsignal.signal(signal.SIGCHLD,signal.SIG_IGN)\nr,w=os.pipe()\nfor _ in range(4):\n if os.fork()==0:\n  os.close(r); start=time.process_time()\n  while time.process_time()-start<0.05: pass\n  os.write(w,b'+'); os._exit(0)\nos.close(w)\nwhile os.read(r,4): pass\nfd=os.open('/dev/zero',os.O_RDONLY);start=time.process_time()\nwhile time.process_time()-start<0.1: os.read(fd,1024*1024)\n";
        let mut command = std::process::Command::new("python3");
        command.args(["-c", script]);
        let Some(pending) = super::prepare(&mut command) else {
            return;
        };
        let child = command.spawn().unwrap();
        let counter = pending.receive();
        let mut status = 0;
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: the owned child has not been reaped, and wait4 initializes usage.
        assert_eq!(
            unsafe { libc::wait4(child.id() as i32, &mut status, 0, usage.as_mut_ptr()) },
            child.id() as i32
        );
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
        let Some(cpu) = counter.and_then(|c| c.cpu_ms()) else {
            return;
        };
        let usage = unsafe { usage.assume_init() };
        let milliseconds = |t: libc::timeval| t.tv_sec as u64 * 1000 + t.tv_usec as u64 / 1000;
        let system = milliseconds(usage.ru_stime);
        let waited = milliseconds(usage.ru_utime) + system;
        assert!(system >= 50, "kernel CPU fixture did not run: {system} ms");
        assert!(
            cpu + 5 >= 180 + waited,
            "auto-reaped tree CPU missing: {cpu} ms, waited {waited} ms"
        );
    }
}
