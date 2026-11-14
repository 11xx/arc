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
    pub struct Counter(OwnedFd);

    pub fn prepare(command: &mut Command) -> Option<Pending> {
        use std::io::{Read, Write};
        let (mut parent, child) = std::os::unix::net::UnixStream::pair().ok()?;
        let worker = std::thread::spawn(move || {
            let mut pid = [0_u8; 4];
            parent.read_exact(&mut pid).ok()?;
            let attr = Attr {
                kind: 0,
                size: std::mem::size_of::<Attr>() as u32,
                config: 1,
                period: 0,
                sample_type: 0,
                read_format: 3,
                flags: 1 | (1 << 1) | (1 << 5) | (1 << 6) | (1 << 12),
                wakeup: 0,
                bp_type: 0,
                config1: 0,
            };
            // SAFETY: attr describes the stopped-before-exec child. Only the
            // holder owns this descriptor; prctl in the gate cannot disable it.
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_perf_event_open,
                    &attr,
                    i32::from_ne_bytes(pid),
                    -1,
                    -1,
                    8,
                ) as i32
            };
            let counter = (fd >= 0).then(|| Counter(unsafe { OwnedFd::from_raw_fd(fd) }));
            parent.write_all(&[0]).ok()?;
            counter
        });
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
                        _ => return Err(std::io::Error::other("counter holder disconnected")),
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

    impl Counter {
        /// A multiplexed or incomplete counter is absent, never a smaller workload.
        pub fn finish(self) -> Option<u64> {
            let mut values = [0_u64; 3];
            // SAFETY: read initializes at most the size of this array.
            let read = unsafe {
                libc::read(
                    self.0.as_raw_fd(),
                    values.as_mut_ptr().cast(),
                    std::mem::size_of_val(&values),
                )
            };
            (read == std::mem::size_of_val(&values) as isize
                && values[1] > 0
                && values[1] == values[2])
                .then_some(values[0])
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
