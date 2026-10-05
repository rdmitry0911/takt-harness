//! The only unsafe code in the crate: two system calls (perf_event_open and wait4).
#![allow(unsafe_code)]

use std::io;
use std::os::fd::{FromRawFd, OwnedFd};

/// perf_event_attr, version 0 layout (64 bytes); every kernel since 2.6.31 accepts this size.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct PerfEventAttr {
    pub type_: u32,
    pub size: u32,
    pub config: u64,
    pub sample_period: u64,
    pub sample_type: u64,
    pub read_format: u64,
    pub flags: u64,
    pub wakeup_events: u32,
    pub bp_type: u32,
    pub config1: u64,
}

/// PERF_FLAG_FD_CLOEXEC: the counter descriptor is not passed to the measured command.
pub const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 1 << 3;

/// perf_event_open(2) for the calling process (`pid = 0`) on any CPU (`cpu = -1`).
pub fn perf_event_open_self(attr: &PerfEventAttr) -> io::Result<OwnedFd> {
    // SAFETY: `attr` is a valid, fully initialised perf_event_attr of `attr.size` bytes that
    // outlives the call; the remaining arguments are plain integers.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_perf_event_open,
            attr as *const PerfEventAttr,
            0 as libc::pid_t,
            -1 as libc::c_int,
            -1 as libc::c_int,
            PERF_FLAG_FD_CLOEXEC,
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the kernel returned a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(ret as libc::c_int) })
}

/// Resource usage of a reaped child (and its reaped descendants).
#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub user_ns: u64,
    pub sys_ns: u64,
    pub max_rss_kib: u64,
}

/// wait4(2): reap `pid`, returning the raw wait status and its resource usage.
pub fn wait4(pid: libc::pid_t) -> io::Result<(libc::c_int, Usage)> {
    let mut status: libc::c_int = 0;
    // SAFETY: rusage is plain old data, for which all-zero bytes are a valid value.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `status` and `ru` are valid writable locations for the duration of the call.
        let r = unsafe { libc::wait4(pid, &mut status, 0, &mut ru) };
        if r == pid {
            break;
        }
        let e = io::Error::last_os_error();
        if r < 0 && e.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(e);
    }
    let tv = |t: libc::timeval| (t.tv_sec as u64) * 1_000_000_000 + (t.tv_usec as u64) * 1_000;
    Ok((status, Usage { user_ns: tv(ru.ru_utime), sys_ns: tv(ru.ru_stime), max_rss_kib: ru.ru_maxrss.max(0) as u64 }))
}
