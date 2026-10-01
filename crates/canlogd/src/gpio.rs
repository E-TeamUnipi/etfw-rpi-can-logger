//! Power-fail input via the GPIO character device (v1 uAPI, no library).
//!
//! Wire a comparator / optocoupler on the 12 V input to a GPIO. When the
//! supply drops, the logger flushes and stops writing until power is back.

use crate::Shared;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::sync::atomic::Ordering::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

const fn ioc(dir: u32, typ: u32, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | (typ << 8) | nr
}
const GPIO_GET_CHIPINFO_IOCTL: u32 = ioc(2, 0xB4, 0x01, 68);
const GPIO_GET_LINEEVENT_IOCTL: u32 = ioc(3, 0xB4, 0x04, 48);
const GPIOHANDLE_GET_LINE_VALUES_IOCTL: u32 = ioc(3, 0xB4, 0x08, 64);
const GPIOHANDLE_REQUEST_INPUT: u32 = 1 << 0;
const GPIOHANDLE_REQUEST_ACTIVE_LOW: u32 = 1 << 2;
const GPIOHANDLE_REQUEST_BIAS_PULL_UP: u32 = 1 << 5;
const GPIOEVENT_REQUEST_BOTH_EDGES: u32 = 3;

#[repr(C)]
struct ChipInfo {
    name: [u8; 32],
    label: [u8; 32],
    lines: u32,
}

#[repr(C)]
struct EventRequest {
    lineoffset: u32,
    handleflags: u32,
    eventflags: u32,
    consumer_label: [u8; 32],
    fd: i32,
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Find the GPIO chip that drives the 40-pin header on Pi 3 / Pi 4 / Pi 5.
fn find_header_chip() -> Option<String> {
    let labels = ["pinctrl-rp1", "pinctrl-bcm2711", "pinctrl-bcm2835"];
    for i in 0..16 {
        let path = format!("/dev/gpiochip{i}");
        let Ok(f) = File::open(&path) else { continue };
        let mut info: ChipInfo = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::ioctl(f.as_raw_fd(), GPIO_GET_CHIPINFO_IOCTL as _, &mut info) };
        if r == 0 && labels.contains(&cstr(&info.label).as_str()) {
            return Some(path);
        }
    }
    None
}

pub struct PfConfig {
    pub chip: String,
    pub line: u32,
    pub active_low: bool,
    pub pull_up: bool,
    pub restore_ms: u64,
}

fn read_value(fd: i32) -> io::Result<bool> {
    let mut data = [0u8; 64];
    let r = unsafe { libc::ioctl(fd, GPIOHANDLE_GET_LINE_VALUES_IOCTL as _, data.as_mut_ptr()) };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(data[0] != 0)
}

/// Start the monitor thread. Line value 1 (after active-low inversion)
/// means "power is failing".
pub fn spawn(cfg: PfConfig, shared: Arc<Shared>) -> io::Result<()> {
    let chip = if cfg.chip == "auto" {
        find_header_chip().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no GPIO header chip found"))?
    } else {
        cfg.chip.clone()
    };
    let f = File::open(&chip)?;
    let mut req = EventRequest {
        lineoffset: cfg.line,
        handleflags: GPIOHANDLE_REQUEST_INPUT
            | if cfg.active_low { GPIOHANDLE_REQUEST_ACTIVE_LOW } else { 0 }
            | if cfg.pull_up { GPIOHANDLE_REQUEST_BIAS_PULL_UP } else { 0 },
        eventflags: GPIOEVENT_REQUEST_BOTH_EDGES,
        consumer_label: [0; 32],
        fd: -1,
    };
    req.consumer_label[..8].copy_from_slice(b"powerfai");
    let mut r = unsafe { libc::ioctl(f.as_raw_fd(), GPIO_GET_LINEEVENT_IOCTL as _, &mut req) };
    if r < 0 && cfg.pull_up {
        // older kernels reject bias flags on the v1 API
        req.handleflags &= !GPIOHANDLE_REQUEST_BIAS_PULL_UP;
        r = unsafe { libc::ioctl(f.as_raw_fd(), GPIO_GET_LINEEVENT_IOCTL as _, &mut req) };
    }
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut ev = unsafe { File::from_raw_fd(req.fd) };
    let fd = req.fd;
    let asserted = read_value(fd)?;
    shared.pf_asserted.store(asserted, Relaxed);
    eprintln!("canlogd: power-fail input {chip} line {} ready (currently {})", cfg.line, if asserted { "FAIL" } else { "ok" });

    std::thread::Builder::new().name("powerfail".into()).spawn(move || {
        let restore = Duration::from_millis(cfg.restore_ms);
        let mut restore_at: Option<Instant> = None;
        loop {
            let timeout = restore_at.map(|t| t.saturating_duration_since(Instant::now()).as_millis() as i32).unwrap_or(1000);
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            unsafe { libc::poll(&mut pfd, 1, timeout.max(0)) };
            if pfd.revents & libc::POLLIN != 0 {
                let mut buf = [0u8; 16 * 16];
                let _ = ev.read(&mut buf);
            }
            let v = match read_value(fd) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let was = shared.pf_asserted.load(Relaxed);
            if v {
                restore_at = None;
                if !was {
                    shared.pf_asserted.store(true, Relaxed);
                    shared.pf_events.fetch_add(1, Relaxed);
                    shared.wake();
                }
            } else if was {
                // require the supply to be back for a while (engine cranking)
                match restore_at {
                    None => restore_at = Some(Instant::now() + restore),
                    Some(t) if Instant::now() >= t => {
                        restore_at = None;
                        shared.pf_asserted.store(false, Relaxed);
                        shared.wake();
                    }
                    _ => {}
                }
            }
        }
    })?;
    Ok(())
}
