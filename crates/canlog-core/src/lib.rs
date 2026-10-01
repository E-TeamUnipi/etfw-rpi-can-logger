//! Shared code for the CAN ring logger: on-disk format, ring access,
//! exporters, configuration and the control protocol.

pub mod config;
pub mod export;
pub mod format;
pub mod proto;
pub mod ring;
pub mod timefmt;

/// Nanoseconds of CLOCK_BOOTTIME (monotonic, includes suspend).
pub fn boot_ns() -> u64 {
    clock_ns(libc::CLOCK_BOOTTIME) as u64
}

/// Nanoseconds of CLOCK_REALTIME.
pub fn realtime_ns() -> i64 {
    clock_ns(libc::CLOCK_REALTIME)
}

fn clock_ns(clk: libc::clockid_t) -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(clk, &mut ts) };
    ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64
}
