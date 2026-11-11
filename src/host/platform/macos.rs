//! macOS: mach for the thread policy and the timer, `~/Library` for the folders.

use std::path::{Path, PathBuf};

#[repr(C)]
struct TimebaseInfo {
    numer: u32,
    denom: u32,
}

extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_wait_until(deadline: u64) -> i32;
    fn mach_timebase_info(info: *mut TimebaseInfo) -> i32;
    fn mach_thread_self() -> u32;
    fn thread_policy_set(thread: u32, flavor: u32, info: *const TimeConstraint, count: u32) -> i32;
}

#[repr(C)]
struct TimeConstraint {
    period: u32,
    computation: u32,
    constraint: u32,
    preemptible: i32,
}

/// The mach timebase (ticks to ns), None if the call fails.
#[allow(unsafe_code)]
fn timebase() -> Option<TimebaseInfo> {
    let mut tb = TimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: a plain mach call filling a correctly sized struct.
    let ok = unsafe { mach_timebase_info(&mut tb) } == 0 && tb.numer != 0;
    ok.then_some(tb)
}

/// The time-constraint policy: a 5 ms period, 0.5 ms of work, 1 ms constraint.
#[allow(unsafe_code)]
pub(super) fn promote_thread() -> bool {
    const THREAD_TIME_CONSTRAINT_POLICY: u32 = 2;
    const COUNT: u32 = 4;
    let Some(tb) = timebase() else {
        return false;
    };
    let abs = |ns: f64| (ns * tb.denom as f64 / tb.numer as f64) as u32;
    let p = TimeConstraint {
        period: abs(5_000_000.0),
        computation: abs(500_000.0),
        constraint: abs(1_000_000.0),
        preemptible: 1,
    };
    // SAFETY: a plain mach call on the current thread with a correctly sized struct.
    unsafe { thread_policy_set(mach_thread_self(), THREAD_TIME_CONSTRAINT_POLICY, &p, COUNT) == 0 }
}

/// `mach_wait_until` an absolute deadline `d` seconds from now; false if the timebase is
/// unavailable (the caller falls back to `std::thread::sleep`).
#[allow(unsafe_code)]
pub(super) fn precise_sleep(d: f64) -> bool {
    let Some(tb) = timebase() else {
        return false;
    };
    let ticks = (d * 1e9 * tb.denom as f64 / tb.numer as f64) as u64;
    // SAFETY: plain mach calls; mach_wait_until only blocks the calling thread.
    unsafe {
        let deadline = mach_absolute_time().saturating_add(ticks);
        // KERN_ABORTED (interrupted): wait again for the same deadline
        while mach_wait_until(deadline) != 0 && mach_absolute_time() < deadline {}
    }
    true
}

pub(super) fn data_dir_in(home: &Path) -> PathBuf {
    home.join("Library/Application Support")
}

pub(super) fn cache_dir_in(home: &Path) -> PathBuf {
    home.join("Library/Caches")
}
