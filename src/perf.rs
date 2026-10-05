//! Hardware counters of a measured command.
//!
//! Counters are opened on the harness itself, disabled, with `inherit` and `enable_on_exec`.
//! The spawned child inherits a copy of each counter, the copy switches on when the child calls
//! exec, and everything the command and its descendants do in user mode is counted. The harness's
//! own copy never runs. When a child exits, the kernel folds its counts into the harness's
//! counter, so one read after the command is reaped gives the total.

use std::fs::File;
use std::io::{self, Read};

use crate::sys::{perf_event_open_self, PerfEventAttr};

const PERF_TYPE_HARDWARE: u32 = 0;

const DISABLED: u64 = 1 << 0;
const INHERIT: u64 = 1 << 1;
const EXCLUDE_KERNEL: u64 = 1 << 5;
const EXCLUDE_HV: u64 = 1 << 6;
const ENABLE_ON_EXEC: u64 = 1 << 12;

const FORMAT_TOTAL_TIME_ENABLED: u64 = 1 << 0;
const FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Cycles,
    Instructions,
    CacheMisses,
    BranchMisses,
}

impl Event {
    pub const ALL: [Event; 4] = [Event::Cycles, Event::Instructions, Event::CacheMisses, Event::BranchMisses];

    /// Key used in the readings bundle.
    pub fn key(self) -> &'static str {
        match self {
            Event::Cycles => "cycles",
            Event::Instructions => "instructions",
            Event::CacheMisses => "cache_misses",
            Event::BranchMisses => "branch_misses",
        }
    }

    /// Whether the measurement needs this event (otherwise it is optional).
    pub fn required(self) -> bool {
        matches!(self, Event::Cycles | Event::Instructions)
    }

    fn config(self) -> u64 {
        match self {
            Event::Cycles => 0,       // PERF_COUNT_HW_CPU_CYCLES
            Event::Instructions => 1, // PERF_COUNT_HW_INSTRUCTIONS
            Event::CacheMisses => 3,  // PERF_COUNT_HW_CACHE_MISSES
            Event::BranchMisses => 5, // PERF_COUNT_HW_BRANCH_MISSES
        }
    }
}

pub struct Counter {
    file: File,
}

/// One read: the count and how long the counter was enabled and actually on the PMU.
#[derive(Clone, Copy, Debug)]
pub struct Reading {
    pub value: u64,
    pub enabled_ns: u64,
    pub running_ns: u64,
}

impl Reading {
    /// The count, scaled up if the kernel multiplexed the counter, and the share of time it ran.
    /// `None` when the counter was enabled but never got onto the PMU.
    pub fn estimate(&self) -> Option<(u64, f64)> {
        if self.enabled_ns == 0 || self.running_ns >= self.enabled_ns {
            return Some((self.value, 1.0));
        }
        if self.running_ns == 0 {
            return None;
        }
        let coverage = self.running_ns as f64 / self.enabled_ns as f64;
        Some(((self.value as f64 / coverage).round() as u64, coverage))
    }
}

impl Counter {
    /// A disabled, inherited, user-mode counter that switches on when a child calls exec.
    pub fn open(event: Event) -> io::Result<Self> {
        let attr = PerfEventAttr {
            type_: PERF_TYPE_HARDWARE,
            size: std::mem::size_of::<PerfEventAttr>() as u32,
            config: event.config(),
            read_format: FORMAT_TOTAL_TIME_ENABLED | FORMAT_TOTAL_TIME_RUNNING,
            flags: DISABLED | INHERIT | EXCLUDE_KERNEL | EXCLUDE_HV | ENABLE_ON_EXEC,
            ..Default::default()
        };
        Ok(Self { file: File::from(perf_event_open_self(&attr)?) })
    }

    pub fn read(&mut self) -> io::Result<Reading> {
        let mut buf = [0u8; 24];
        self.file.read_exact(&mut buf)?;
        let word = |i: usize| u64::from_ne_bytes(buf[i * 8..i * 8 + 8].try_into().unwrap());
        Ok(Reading { value: word(0), enabled_ns: word(1), running_ns: word(2) })
    }
}

/// kernel.perf_event_paranoid, if readable.
pub fn paranoid() -> Option<i32> {
    std::fs::read_to_string("/proc/sys/kernel/perf_event_paranoid").ok()?.trim().parse().ok()
}

/// A human explanation of why a counter could not be opened.
pub fn explain(event: Event, err: &io::Error) -> String {
    let base = format!("perf_event_open({}) failed: {}", event.key(), err);
    let why = match err.raw_os_error() {
        Some(libc::EACCES) | Some(libc::EPERM) => match paranoid() {
            Some(p) if p > 2 => format!(
                "kernel.perf_event_paranoid is {p}; user-mode counting of your own processes needs 2 or less \
                 (sudo sysctl kernel.perf_event_paranoid=2)"
            ),
            _ => "permission denied: a container or sandbox seccomp profile probably blocks perf_event_open \
                  (Docker: --cap-add PERFMON or a seccomp profile that allows it)"
                .to_string(),
        },
        Some(libc::ENOENT) | Some(libc::ENODEV) | Some(libc::EOPNOTSUPP) => {
            "no hardware PMU is visible: a virtual machine without a virtual PMU (vPMU), or an unsupported CPU"
                .to_string()
        }
        Some(libc::ENOSYS) => "the kernel has no perf_event support".to_string(),
        Some(libc::EINVAL) => "the kernel rejected the counter configuration".to_string(),
        _ => String::new(),
    };
    if why.is_empty() {
        base
    } else {
        format!("{base}; {why}")
    }
}
