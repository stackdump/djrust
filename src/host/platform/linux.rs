//! Linux: pthread / prctl for the thread, `clock_nanosleep` for the timer, XDG for the folders.

use std::path::{Path, PathBuf};

/// `SCHED_FIFO` at priority 20 (needs `rtprio` in limits.conf, or rtkit), and a 1 ns timer
/// slack (default 50 us) for the precise sleep whether or not that is granted.
#[allow(unsafe_code)]
pub(super) fn promote_thread() -> bool {
    // SAFETY: sched_param is plain data; both calls affect only this thread.
    unsafe {
        libc::prctl(libc::PR_SET_TIMERSLACK, 1 as libc::c_ulong);
        let p = libc::sched_param { sched_priority: 20 };
        libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &p) == 0
    }
}

/// `clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME)` to `d` seconds from now, again on EINTR;
/// false if the clock cannot be read (the caller falls back to `std::thread::sleep`).
#[allow(unsafe_code)]
pub(super) fn precise_sleep(d: f64) -> bool {
    // SAFETY: timespec is plain data; clock_nanosleep only blocks the calling thread.
    unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        if libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) != 0 {
            return false;
        }
        let ns = ts.tv_nsec as i64 + (d * 1e9) as i64;
        ts.tv_sec += (ns / 1_000_000_000) as libc::time_t;
        ts.tv_nsec = (ns % 1_000_000_000) as _;
        while libc::clock_nanosleep(
            libc::CLOCK_MONOTONIC,
            libc::TIMER_ABSTIME,
            &ts,
            std::ptr::null_mut(),
        ) == libc::EINTR
        {}
    }
    true
}

pub(super) fn data_dir_in(home: &Path) -> PathBuf {
    super::xdg("XDG_DATA_HOME", home, ".local/share")
}

pub(super) fn cache_dir_in(home: &Path) -> PathBuf {
    super::xdg("XDG_CACHE_HOME", home, ".cache")
}
