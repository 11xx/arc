//! Signals that end arc while it owns a child's process group.
//!
//! A gate or probe leads its own process group, so a terminal's SIGINT does
//! not reach it, and arc dying of the signal would leave the group running.
//! While a [`Watch`] is held, SIGINT, SIGTERM and SIGHUP are recorded instead
//! of ending arc. The wait loop that owns the group sees the record, stops
//! the group and returns [`Interrupted`], and `main` then ends arc with the
//! same signal. A signal arc inherited as ignored stays ignored, and with no
//! watch held every signal keeps its default disposition.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// The first watched signal received, or 0.
static RECEIVED: AtomicI32 = AtomicI32::new(0);

/// Live watches, and the dispositions the first one replaced.
static WATCHES: Mutex<(usize, Vec<(libc::c_int, libc::sigaction)>)> = Mutex::new((0, Vec::new()));

extern "C" fn record(signal: libc::c_int) {
    let _ = RECEIVED.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
}

/// Holds the watched signals' handlers while an owned child runs.
pub(crate) struct Watch(());

impl Watch {
    pub(crate) fn start() -> std::io::Result<Watch> {
        let mut watches = WATCHES.lock().unwrap_or_else(|poison| poison.into_inner());
        if watches.0 == 0 {
            for signal in SIGNALS {
                // SAFETY: a zeroed sigaction is a valid empty action, and
                // sigaction only reads `action` and writes `previous`.
                let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
                let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
                action.sa_sigaction = record as extern "C" fn(libc::c_int) as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                if unsafe { libc::sigaction(signal, std::ptr::null(), &mut previous) } != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if previous.sa_sigaction == libc::SIG_IGN {
                    continue;
                }
                if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                watches.1.push((signal, previous));
            }
        }
        watches.0 += 1;
        Ok(Watch(()))
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let mut watches = WATCHES.lock().unwrap_or_else(|poison| poison.into_inner());
        watches.0 -= 1;
        if watches.0 == 0 {
            for (signal, previous) in watches.1.drain(..) {
                // SAFETY: restores the action sigaction itself reported.
                unsafe { libc::sigaction(signal, &previous, std::ptr::null_mut()) };
            }
        }
    }
}

/// Refuse to go on once a watched signal has been received.
pub(crate) fn check() -> Result<(), Interrupted> {
    match RECEIVED.load(Ordering::SeqCst) {
        0 => Ok(()),
        signal => Err(Interrupted { signal }),
    }
}

/// A run arc stopped because a watched signal reached it.
#[derive(Debug)]
pub(crate) struct Interrupted {
    signal: libc::c_int,
}

impl Interrupted {
    fn name(&self) -> &'static str {
        match self.signal {
            libc::SIGINT => "SIGINT",
            libc::SIGTERM => "SIGTERM",
            libc::SIGHUP => "SIGHUP",
            _ => "a signal",
        }
    }

    /// End arc with the received signal, so its caller sees it interrupted
    /// rather than failed.
    pub(crate) fn end_process(&self) -> ! {
        // SAFETY: restores the default action, unblocks the signal and
        // raises it; none of these calls borrow memory past their return.
        unsafe {
            libc::signal(self.signal, libc::SIG_DFL);
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, self.signal);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            libc::raise(self.signal);
        }
        std::process::exit(128 + self.signal)
    }
}

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "interrupted by {}: arc stopped the process group it was running and recorded no \
             result from it",
            self.name()
        )
    }
}

impl std::error::Error for Interrupted {}
