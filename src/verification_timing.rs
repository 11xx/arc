//! Observations of a command, separate from admission and preparation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerificationTiming {
    #[serde(default, skip_serializing_if = "is_false")]
    pub work_copied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_conditions: Option<Box<crate::gate_work::Conditions>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_tree: Option<Box<crate::gate_work::Tree>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_cpu: Option<Box<crate::gate_work::Host>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_busy_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_capacity_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_identity: Option<Box<crate::gate_work::Identity>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_reference: Option<Box<crate::gate_work::Reference>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_timings: Option<crate::check_timings::Receipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrent_batch: Option<crate::gate_concurrency::Receipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_limit_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_limit_origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_at_run: Option<Box<crate::gate_budget::BudgetJudgement>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepare_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_user_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_system_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_average: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_count: Option<u64>,
}

/// The holder's command observations. Its own CPU is excluded.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct CommandReport {
    #[serde(default)]
    pub stopped: bool,
    pub started_ns: Option<u64>,
    pub status: Option<i32>,
    pub timing: VerificationTiming,
}

pub(crate) fn monotonic_ns() -> Option<u64> {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: clock_gettime initializes the supplied timespec on success.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, time.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: clock_gettime succeeded.
    let time = unsafe { time.assume_init() };
    u64::try_from(time.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(time.tv_nsec).ok()?)
}

pub(crate) fn host_at_start() -> VerificationTiming {
    let mut load = [0.0; 3];
    // SAFETY: getloadavg writes at most three doubles into this array;
    // sysconf takes no borrowed memory.
    let (loads, cores) = unsafe {
        (
            libc::getloadavg(load.as_mut_ptr(), 3),
            libc::sysconf(libc::_SC_NPROCESSORS_ONLN),
        )
    };
    VerificationTiming {
        load_average: (loads == 3).then_some(load),
        core_count: u64::try_from(cores).ok().filter(|cores| *cores > 0),
        ..Default::default()
    }
}

/// Reap one child, including usage of descendants that child waited for.
pub(crate) fn wait_usage(pid: i32, flags: i32) -> std::io::Result<Option<(i32, u64, u64)>> {
    loop {
        let mut status = 0;
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: wait4 initializes status and usage when it returns a PID.
        let waited = unsafe { libc::wait4(pid, &mut status, flags, usage.as_mut_ptr()) };
        if waited > 0 {
            // SAFETY: wait4 returned a child PID.
            let usage = unsafe { usage.assume_init() };
            let micros =
                |time: libc::timeval| (time.tv_sec as u64) * 1_000_000 + time.tv_usec as u64;
            return Ok(Some((
                status,
                micros(usage.ru_utime),
                micros(usage.ru_stime),
            )));
        }
        if waited == 0 {
            return Ok(None);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Distinct package/core pairs exposed by the host; absent without topology data.
pub(crate) fn physical_core_count() -> Option<usize> {
    let text = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    let mut cores = std::collections::BTreeSet::new();
    for processor in text.split("\n\n").filter(|block| !block.trim().is_empty()) {
        let fields = processor
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.trim(), value.trim()))
            .collect::<std::collections::BTreeMap<_, _>>();
        cores.insert((
            fields.get("physical id")?.parse::<u64>().ok()?,
            fields.get("core id")?.parse::<u64>().ok()?,
        ));
    }
    (!cores.is_empty()).then_some(cores.len())
}

fn is_false(value: &bool) -> bool {
    !value
}
