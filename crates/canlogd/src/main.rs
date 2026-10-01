//! canlogd: logs every frame from every SocketCAN interface to a raw ring
//! partition. See README.md for the design.
//!
//! Threads:
//!   main      receive frames, build blocks, handle commands, publish status
//!   writer    write finished blocks with O_DIRECT|O_DSYNC
//!   ctl       Unix socket for the web / BLE / CLI clients
//!   powerfail GPIO power-fail input (optional)
//!   watchdog  feeds /dev/watchdog while main and writer are alive (optional)

mod can;
mod ctl;
mod gpio;
mod ifaces;
mod rx;
mod sim;
mod writer;

use canlog_core::config::{Config, DEFAULT_CTL_SOCKET, DEFAULT_PATH};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering::*};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// State shared between threads.
pub struct Shared {
    pub stop: AtomicBool,
    pub wake_fd: i32,
    pub pf_asserted: AtomicBool,
    pub pf_events: AtomicU32,
    pub state: AtomicU8,
    pub head_seq: AtomicU64,
    pub ring_blocks: AtomicU64,
    pub session_base: AtomicU32,
    pub written_blocks: AtomicU64,
    pub write_errors: AtomicU64,
    pub lost_blocks: AtomicU64,
    pub rx_heartbeat: AtomicU64,
    pub writer_heartbeat: AtomicU64,
    pub writer_done: AtomicBool,
    pub status_json: Mutex<String>,
    pub live: Mutex<Vec<serde_json::Value>>,
    pub sim: bool,
}

impl Shared {
    /// Wake the main loop (safe to call from any thread).
    pub fn wake(&self) {
        let one: u64 = 1;
        unsafe { libc::write(self.wake_fd, &one as *const u64 as *const libc::c_void, 8) };
    }
}

static SIGNAL_WAKE_FD: AtomicI32 = AtomicI32::new(-1);
static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, SeqCst);
    let fd = SIGNAL_WAKE_FD.load(SeqCst);
    if fd >= 0 {
        let one: u64 = 1;
        unsafe { libc::write(fd, &one as *const u64 as *const libc::c_void, 8) };
    }
}

struct Args {
    config: PathBuf,
    ring: Option<PathBuf>,
    ctl: PathBuf,
    sim: bool,
}

fn parse_args() -> Args {
    let mut a = Args { config: DEFAULT_PATH.into(), ring: None, ctl: DEFAULT_CTL_SOCKET.into(), sim: false };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "-c" | "--config" => a.config = it.next().expect("missing value").into(),
            "-r" | "--ring" => a.ring = Some(it.next().expect("missing value").into()),
            "-s" | "--socket" => a.ctl = it.next().expect("missing value").into(),
            "--sim" => a.sim = true,
            "-V" | "--version" => {
                println!("canlogd {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => {
                eprintln!("usage: canlogd [-c logger.conf] [-r ring-device] [-s control.sock] [--sim]");
                std::process::exit(2);
            }
        }
    }
    a
}

fn start_watchdog(shared: Arc<Shared>, timeout_s: i32) {
    const WDIOC_SETTIMEOUT: u32 = (3 << 30) | (4 << 16) | ((b'W' as u32) << 8) | 6;
    let fd = unsafe { libc::open(c"/dev/watchdog".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        eprintln!("canlogd: watchdog: {}", std::io::Error::last_os_error());
        return;
    }
    let mut t = timeout_s;
    unsafe { libc::ioctl(fd, WDIOC_SETTIMEOUT as _, &mut t) };
    eprintln!("canlogd: hardware watchdog armed ({t}s)");
    let _ = std::thread::Builder::new().name("watchdog".into()).spawn(move || loop {
        let now = canlog_core::boot_ns();
        let fresh = |hb: &AtomicU64| now.saturating_sub(hb.load(Relaxed)) < 10_000_000_000;
        if shared.writer_done.load(Relaxed) {
            // clean shutdown: disarm ("magic close")
            unsafe {
                libc::write(fd, b"V".as_ptr() as *const libc::c_void, 1);
                libc::close(fd);
            }
            return;
        }
        if fresh(&shared.rx_heartbeat) && fresh(&shared.writer_heartbeat) {
            unsafe { libc::write(fd, b"k".as_ptr() as *const libc::c_void, 1) };
        }
        std::thread::sleep(Duration::from_secs(2));
    });
}

fn main() {
    let args = parse_args();
    let cfg = Config::load(&args.config);
    let ring_path = args.ring.clone().unwrap_or_else(|| cfg.ring_dev().into());

    // lock memory so paging can never stall frame reception
    unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) };

    let wake_fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
    assert!(wake_fd >= 0, "eventfd failed");
    SIGNAL_WAKE_FD.store(wake_fd, SeqCst);
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        wake_fd,
        pf_asserted: AtomicBool::new(false),
        pf_events: AtomicU32::new(0),
        state: AtomicU8::new(writer::ST_STARTING),
        head_seq: AtomicU64::new(0),
        ring_blocks: AtomicU64::new(0),
        session_base: AtomicU32::new(0),
        written_blocks: AtomicU64::new(0),
        write_errors: AtomicU64::new(0),
        lost_blocks: AtomicU64::new(0),
        rx_heartbeat: AtomicU64::new(canlog_core::boot_ns()),
        writer_heartbeat: AtomicU64::new(canlog_core::boot_ns()),
        writer_done: AtomicBool::new(false),
        status_json: Mutex::new("{}".into()),
        live: Mutex::new(Vec::new()),
        sim: args.sim,
    });

    let ram_mb = cfg.u64_or("ram_buffer_mb", 32).clamp(1, 1024) as usize;
    let pipe = Arc::new(writer::Pipeline::new(ram_mb * 256));

    // the receiver starts first: nothing is lost while the ring is scanned
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let mut rec = rx::Recorder::new(&cfg, shared.clone(), pipe.clone(), cmd_rx);

    {
        let (p, s, path) = (pipe.clone(), shared.clone(), ring_path.clone());
        std::thread::Builder::new().name("writer".into()).spawn(move || writer::run(path, p, s)).unwrap();
    }

    if let Err(e) = ctl::spawn(&args.ctl, shared.clone(), cmd_tx) {
        eprintln!("canlogd: control socket {}: {e}", args.ctl.display());
    }

    if let Some(line) = cfg.get("powerfail_gpio").and_then(|v| v.parse().ok()) {
        let pf = gpio::PfConfig {
            chip: cfg.str_or("powerfail_chip", "auto"),
            line,
            active_low: cfg.bool_or("powerfail_active_low", true),
            pull_up: cfg.bool_or("powerfail_pull_up", false),
            restore_ms: cfg.u64_or("powerfail_restore_ms", 500),
        };
        if let Err(e) = gpio::spawn(pf, shared.clone()) {
            eprintln!("canlogd: power-fail input disabled: {e}");
        }
    }

    if cfg.bool_or("watchdog", false) && !args.sim {
        start_watchdog(shared.clone(), cfg.u64_or("watchdog_timeout_s", 15) as i32);
    }

    rec.run(|| SIGNALLED.load(Relaxed));

    // drain: let the writer finish the queue (bounded wait)
    shared.stop.store(true, Relaxed);
    pipe.notify();
    for _ in 0..50 {
        if shared.writer_done.load(Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_file(&args.ctl);
    eprintln!("canlogd: stopped");
}
