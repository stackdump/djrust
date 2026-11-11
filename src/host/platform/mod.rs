//! What differs between macOS and Linux, in one place: each OS's file next to this one
//! (`macos.rs`, `linux.rs`, `other.rs` for any other target) has the same functions, and
//! the rest of djhost calls them through here, so a platform `cfg` lives in this folder and
//! nowhere else.
//!
//! The choice is made at compile time by the target (`cargo build` on the Mac builds the macOS
//! file, on Linux the Linux one); nothing in here is a runtime switch. What it covers:
//!
//! - [`promote_thread`]: real-time scheduling for the MIDI clock's thread (the time-constraint
//!   policy on macOS, `SCHED_FIFO` on Linux).
//! - [`sleep_for`]: a sleep on the OS's most precise timer (`mach_wait_until`,
//!   `clock_nanosleep(TIMER_ABSTIME)`).
//! - [`data_dir_in`] / [`cache_dir_in`]: where per-user data and caches go
//!   (`~/Library/Application Support`, `~/Library/Caches`; `$XDG_DATA_HOME`, `$XDG_CACHE_HOME`
//!   and their defaults elsewhere).
//! - [`IS_MACOS`]: for code that takes the platform as a parameter so a test can pass either.
//!
//! Unix sockets (`cfg(unix)`, the control socket and the MCP bridge) are a different axis, Unix
//! against not, and stay where they are used.
//!
//! These are the crate's only `unsafe` (OS scheduling and timer calls); see `lib.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod other;

#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use other as imp;

/// Built for macOS.
pub const IS_MACOS: bool = cfg!(target_os = "macos");

/// Ask for real-time scheduling for the calling thread: the time-constraint policy on macOS
/// (a 5 ms period, 0.5 ms of work, 1 ms constraint), `SCHED_FIFO` at a modest priority on Linux
/// (and a 1 ns timer slack there, granted or not, for [`sleep_for`]). True if the OS granted
/// the real-time scheduling; false elsewhere.
pub fn promote_thread() -> bool {
    imp::promote_thread()
}

/// Sleep `d` seconds on the OS's most precise timer. The deadline is absolute
/// (`mach_wait_until` on macOS, `clock_nanosleep(TIMER_ABSTIME)` on Linux), so a sleep cut
/// short by a signal resumes towards the same instant and nothing accumulates. Elsewhere, or if
/// the OS call cannot be set up, `std::thread::sleep`. Nothing for `d <= 0` or NaN.
pub fn sleep_for(d: f64) {
    if d.is_nan() || d <= 0.0 {
        return;
    }
    if !imp::precise_sleep(d) {
        std::thread::sleep(Duration::from_secs_f64(d));
    }
}

/// The per-user data folder under `home`: `~/Library/Application Support` on macOS,
/// `$XDG_DATA_HOME` (else `~/.local/share`) elsewhere.
pub fn data_dir_in(home: &Path) -> PathBuf {
    imp::data_dir_in(home)
}

/// The per-user cache folder under `home`: `~/Library/Caches` on macOS, `$XDG_CACHE_HOME`
/// (else `~/.cache`) elsewhere.
pub fn cache_dir_in(home: &Path) -> PathBuf {
    imp::cache_dir_in(home)
}

/// `$var` when set and non-empty, else `home/fallback`: the XDG rule, shared by Linux and the
/// other non-macOS targets.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn xdg(var: &str, home: &Path, fallback: &str) -> PathBuf {
    match std::env::var_os(var).filter(|d| !d.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => home.join(fallback),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_sleep_sleeps_about_that_long_and_a_past_one_returns() {
        let t = std::time::Instant::now();
        sleep_for(0.002);
        let s = t.elapsed().as_secs_f64();
        assert!((0.0015..0.2).contains(&s), "{s}");
        let t = std::time::Instant::now();
        sleep_for(-1.0);
        sleep_for(f64::NAN);
        assert!(t.elapsed().as_secs_f64() < 0.01);
    }

    #[test]
    fn the_folders_follow_the_platform() {
        let home = Path::new("/home/m");
        if IS_MACOS {
            assert_eq!(data_dir_in(home), Path::new("/home/m/Library/Application Support"));
            assert_eq!(cache_dir_in(home), Path::new("/home/m/Library/Caches"));
        } else {
            // the XDG variables decide when set; this only checks the shape of the defaults
            let d = data_dir_in(home);
            let c = cache_dir_in(home);
            match std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
                Some(x) => assert_eq!(d, PathBuf::from(x)),
                None => assert_eq!(d, Path::new("/home/m/.local/share")),
            }
            match std::env::var_os("XDG_CACHE_HOME").filter(|d| !d.is_empty()) {
                Some(x) => assert_eq!(c, PathBuf::from(x)),
                None => assert_eq!(c, Path::new("/home/m/.cache")),
            }
        }
    }
}
