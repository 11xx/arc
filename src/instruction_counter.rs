//! A counter installed before exec, inherited by every thread and descendant.

#[cfg(target_os = "linux")]
mod linux {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixDatagram;
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

    /// The socket transfers ownership of the child's counter without an exec race.
    pub struct Pending {
        socket: UnixDatagram,
    }
    pub struct Counter(OwnedFd);

    pub fn prepare(command: &mut Command) -> Option<Pending> {
        let (parent, child) = UnixDatagram::pair().ok()?;
        // SAFETY: the child uses stack storage and async-signal-safe syscalls only.
        unsafe {
            command.pre_exec(move || {
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
                let fd = libc::syscall(libc::SYS_perf_event_open, &attr, 0, -1, -1, 8) as i32;
                let mut byte = [0_u8];
                let mut iov = libc::iovec {
                    iov_base: byte.as_mut_ptr().cast(),
                    iov_len: 1,
                };
                let mut control = [0_usize; 4];
                let mut message: libc::msghdr = std::mem::zeroed();
                message.msg_iov = &mut iov;
                message.msg_iovlen = 1;
                if fd >= 0 {
                    message.msg_control = control.as_mut_ptr().cast();
                    message.msg_controllen =
                        libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) as usize;
                    let header = libc::CMSG_FIRSTHDR(&message);
                    (*header).cmsg_level = libc::SOL_SOCKET;
                    (*header).cmsg_type = libc::SCM_RIGHTS;
                    (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as usize;
                    std::ptr::write(libc::CMSG_DATA(header).cast::<i32>(), fd);
                }
                let _ = libc::sendmsg(child.as_raw_fd(), &message, libc::MSG_NOSIGNAL);
                if fd >= 0 {
                    libc::close(fd);
                }
                Ok(())
            });
        }
        Some(Pending { socket: parent })
    }

    impl Pending {
        pub fn receive(self) -> Option<Counter> {
            let mut byte = [0_u8];
            let mut iov = libc::iovec {
                iov_base: byte.as_mut_ptr().cast(),
                iov_len: 1,
            };
            let mut control = [0_usize; 4];
            // SAFETY: recvmsg initializes the supplied message buffers; a received
            // SCM_RIGHTS descriptor is owned by this process.
            unsafe {
                let mut message: libc::msghdr = std::mem::zeroed();
                message.msg_iov = &mut iov;
                message.msg_iovlen = 1;
                message.msg_control = control.as_mut_ptr().cast();
                message.msg_controllen = std::mem::size_of_val(&control);
                if libc::recvmsg(
                    self.socket.as_raw_fd(),
                    &mut message,
                    libc::MSG_CMSG_CLOEXEC,
                ) != 1
                {
                    return None;
                }
                let header = libc::CMSG_FIRSTHDR(&message);
                if header.is_null()
                    || (*header).cmsg_level != libc::SOL_SOCKET
                    || (*header).cmsg_type != libc::SCM_RIGHTS
                {
                    return None;
                }
                let fd = std::ptr::read(libc::CMSG_DATA(header).cast::<i32>());
                Some(Counter(OwnedFd::from_raw_fd(fd)))
            }
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
