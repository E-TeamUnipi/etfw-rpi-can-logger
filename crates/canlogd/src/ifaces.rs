//! CAN interface discovery and bring-up. Polls /sys/class/net, identifies
//! adapters by USB serial / port, applies the bitrate from logger.conf and
//! sets the interface up. No udev needed.

use canlog_core::config::{parse_num, Config};
use canlog_core::format::{ifstate, IfaceInfo};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

pub struct Iface {
    pub idx: u8,
    pub ifindex: i32,
    pub info: IfaceInfo,
    pub present: bool,
    pub state: u8,
    pub config_error: Option<String>,
    // statistics
    pub frames: u64,
    pub errors: u64,
    pub frames_prev: u64,
    pub busy_ns: u64,
    pub fps: f64,
    pub load: f64,
}

#[derive(Default)]
pub struct IfaceTable {
    pub list: Vec<Iface>,
    by_ifindex: HashMap<i32, usize>,
}

pub enum Event {
    /// New or re-appeared interface: log its description.
    Added(usize),
    /// Interface disappeared.
    Removed(usize),
}

fn read(p: impl AsRef<Path>) -> String {
    fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default()
}

struct Identity {
    serial: String,
    port: String,
    driver: String,
    channel: u32,
    virtual_if: bool,
}

fn identify(name: &str) -> Identity {
    let base = Path::new("/sys/class/net").join(name);
    let dev = base.join("device");
    let driver = fs::read_link(dev.join("driver"))
        .ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let mut serial = String::new();
    let mut port = String::new();
    if let Ok(real) = fs::canonicalize(&dev) {
        // USB interface dir is like .../1-1.3:1.0 ; its parent is the device
        let mut d = real.as_path();
        for _ in 0..3 {
            if d.join("idVendor").exists() {
                serial = read(d.join("serial"));
                port = d.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                break;
            }
            match d.parent() {
                Some(p) => d = p,
                None => break,
            }
        }
    }
    let dev_port = read(base.join("dev_port")).parse::<u32>().unwrap_or(0);
    let dev_id = u32::from_str_radix(read(base.join("dev_id")).trim_start_matches("0x"), 16).unwrap_or(0);
    Identity { serial, port, driver, channel: dev_port.max(dev_id), virtual_if: !dev.exists() }
}

fn ip(args: &[&str]) -> Result<(), String> {
    let out = Command::new("ip").args(args).output().map_err(|e| format!("ip: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Apply logger.conf settings and bring the interface up.
fn configure(cfg: &Config, name: &str, id: &Identity) -> (IfaceInfo, Option<String>) {
    let ch = id.channel;
    let serial_ch = if id.serial.is_empty() { String::new() } else { format!("{}_{ch}", id.serial) };
    let port_ch = if id.port.is_empty() { String::new() } else { format!("{}_{ch}", id.port) };
    let ids = [serial_ch.as_str(), id.serial.as_str(), port_ch.as_str(), id.port.as_str(), name];
    let get = |k: &str| cfg.can_setting(&ids, k);
    let num = |k: &str, d: u64| get(k).and_then(parse_num).unwrap_or(d);
    let flag = |k: &str, d: bool| match get(k).map(|s| s.to_ascii_lowercase()) {
        Some(s) => matches!(s.as_str(), "1" | "on" | "yes" | "true"),
        None => d,
    };

    let mut info = IfaceInfo {
        name: name.to_string(),
        label: get("label").unwrap_or("").to_string(),
        serial: id.serial.clone(),
        usb_port: id.port.clone(),
        driver: id.driver.clone(),
        bitrate: num("bitrate", 500_000) as u32,
        dbitrate: num("dbitrate", 0) as u32,
        fd: false,
        listen_only: flag("listen_only", true),
    };
    info.fd = info.dbitrate > 0;

    if id.virtual_if {
        // vcan / vxcan: no bit timing
        info.bitrate = 0;
        info.dbitrate = 0;
        let _ = ip(&["link", "set", "dev", name, "up"]);
        return (info, None);
    }
    if !flag("autoconfig", true) {
        let _ = ip(&["link", "set", "dev", name, "up"]);
        return (info, None);
    }

    let br = info.bitrate.to_string();
    let dbr = info.dbitrate.to_string();
    let sp = get("sample_point").unwrap_or("").to_string();
    let restart = num("restart_ms", 100).to_string();
    let mut args: Vec<&str> = vec!["link", "set", "dev", name, "type", "can", "bitrate", &br];
    if !sp.is_empty() {
        args.extend(["sample-point", &sp]);
    }
    if info.fd {
        args.extend(["dbitrate", &dbr, "fd", "on"]);
    }
    args.extend(["listen-only", if info.listen_only { "on" } else { "off" }]);
    args.extend(["restart-ms", &restart]);

    let _ = ip(&["link", "set", "dev", name, "down"]);
    let mut err = ip(&args).err();
    if err.is_some() && info.listen_only {
        // some adapters do not support listen-only; retry without it
        let pos = args.iter().position(|a| *a == "listen-only").unwrap();
        args.drain(pos..pos + 2);
        if ip(&args).is_ok() {
            err = Some("adapter does not support listen-only; running in normal mode (it will ACK frames)".into());
            info.listen_only = false;
        }
    }
    if let Err(e) = ip(&["link", "set", "dev", name, "up"]) {
        err = Some(match err {
            Some(prev) => format!("{prev}; up: {e}"),
            None => format!("up: {e}"),
        });
    }
    (info, err)
}

impl IfaceTable {
    pub fn get_idx(&self, ifindex: i32) -> Option<u8> {
        self.by_ifindex.get(&ifindex).map(|&i| self.list[i].idx)
    }

    pub fn by_idx_mut(&mut self, idx: u8) -> Option<&mut Iface> {
        self.list.iter_mut().find(|i| i.idx == idx)
    }

    fn insert(&mut self, ifindex: i32, info: IfaceInfo, err: Option<String>) -> usize {
        // re-plugged adapter: reuse its index within the session
        let slot = self.list.iter().position(|i| !i.present && i.info.name == info.name && i.info.serial == info.serial && i.info.usb_port == info.usb_port);
        let i = match slot {
            Some(s) => {
                let e = &mut self.list[s];
                self.by_ifindex.retain(|_, v| *v != s);
                e.ifindex = ifindex;
                e.info = info;
                e.present = true;
                e.state = ifstate::UP;
                e.config_error = err;
                s
            }
            None => {
                let idx = self.list.len().min(254) as u8;
                self.list.push(Iface {
                    idx,
                    ifindex,
                    info,
                    present: true,
                    state: ifstate::UP,
                    config_error: err,
                    frames: 0,
                    errors: 0,
                    frames_prev: 0,
                    busy_ns: 0,
                    fps: 0.0,
                    load: 0.0,
                });
                self.list.len() - 1
            }
        };
        self.by_ifindex.insert(ifindex, i);
        i
    }

    /// Add a simulated interface (no sysfs).
    pub fn add_sim(&mut self, ifindex: i32, info: IfaceInfo) -> usize {
        self.insert(ifindex, info, None)
    }

    /// Scan /sys/class/net for CAN interfaces.
    pub fn scan(&mut self, cfg: &Config) -> Vec<Event> {
        let mut events = Vec::new();
        let mut seen = Vec::new();
        if let Ok(rd) = fs::read_dir("/sys/class/net") {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let base = e.path();
                if read(base.join("type")) != "280" {
                    continue; // not ARPHRD_CAN
                }
                let Ok(ifindex) = read(base.join("ifindex")).parse::<i32>() else { continue };
                seen.push(ifindex);
                if self.by_ifindex.contains_key(&ifindex) {
                    continue;
                }
                let id = identify(&name);
                let (info, err) = configure(cfg, &name, &id);
                if let Some(e) = &err {
                    eprintln!("canlogd: {name}: {e}");
                }
                eprintln!(
                    "canlogd: {name} up: {} bit/s{} driver={} serial={} port={}",
                    info.bitrate,
                    if info.listen_only { " listen-only" } else { "" },
                    info.driver,
                    info.serial,
                    info.usb_port
                );
                let i = self.insert(ifindex, info, err);
                events.push(Event::Added(i));
            }
        }
        for (i, it) in self.list.iter_mut().enumerate() {
            if it.present && it.ifindex > 0 && !seen.contains(&it.ifindex) {
                it.present = false;
                it.state = ifstate::REMOVED;
                events.push(Event::Removed(i));
            }
        }
        events
    }
}

pub fn state_name(s: u8) -> &'static str {
    match s {
        ifstate::DOWN => "down",
        ifstate::UP => "up",
        ifstate::REMOVED => "unplugged",
        ifstate::ERROR_WARNING => "error-warning",
        ifstate::ERROR_PASSIVE => "error-passive",
        ifstate::BUS_OFF => "bus-off",
        ifstate::ERROR_ACTIVE => "ok",
        _ => "?",
    }
}
