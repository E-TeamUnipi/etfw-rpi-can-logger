//! The main loop: receive frames, pack them into blocks, handle commands,
//! power-fail transitions and periodic housekeeping, publish status.

use crate::can::{CanSocket, RxFrame, TxSocket};
use crate::ifaces::{self, Event, IfaceTable};
use crate::sim::{self, Sim};
use crate::writer::{self, Pipeline};
use crate::{Request, Shared};
use canlog_core::config::Config;
use canlog_core::format::*;
use canlog_core::proto::{self, err, Command};
use canlog_core::ring::AlignedBuf;
use canlog_core::{boot_ns, realtime_ns};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::Ordering::*;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

const MS: u64 = 1_000_000;
const SEC: u64 = 1_000_000_000;
const CHECKPOINT_NS: u64 = 30 * SEC;
const LIVE_MAX: usize = 4096;
/// 2025-01-01: anything earlier means the clock was never set
const VALID_TIME_NS: i64 = 1_735_689_600 * 1_000_000_000;

struct Live {
    fd: bool,
    len: u8,
    data: [u8; 64],
    count: u64,
    count_prev: u64,
    hz: f64,
    last_ts: u64,
}

pub struct Recorder {
    cfg: Config,
    shared: Arc<Shared>,
    pipe: Arc<Pipeline>,
    cmds: Receiver<Request>,
    sock: Option<CanSocket>,
    tx_sock: Option<TxSocket>,
    /// binary records for stream clients, sent once per batch of frames
    stream_buf: Vec<u8>,
    sock_error: Option<String>,
    sim: Option<Sim>,
    ifaces: IfaceTable,
    // block being filled
    cur: Option<AlignedBuf>,
    builder: BlockBuilder,
    block_opened: u64,
    flush_ns: u64,
    // session
    session_ord: u32,
    session_start_flag: bool,
    session_started_at: u64,
    name: String,
    time: Option<(i64, u8)>,
    set_clock: bool,
    dropped_total: u32,
    marks: u32,
    // stats
    live: HashMap<(u8, u32), Live>,
    frames_total: u64,
    frames_prev: u64,
    fps: f64,
    written_prev: u64,
    write_bps: f64,
    pf_prev: bool,
    rt_off: i64,
    t_checkpoint: u64,
    t_scan: u64,
    t_publish: u64,
    t_sock_retry: u64,
    frames: Vec<RxFrame>,
}

fn bits_ns(f: &RxFrame, bitrate: u32, dbitrate: u32) -> u64 {
    if bitrate == 0 {
        return 0;
    }
    let ext = f.id & CAN_EFF_FLAG != 0;
    if f.fd {
        let arb = if ext { 50 } else { 30 } as u64;
        let data_bits = 8 * f.len as u64 + 40;
        let dr = if f.flags & CANFD_BRS != 0 && dbitrate > 0 { dbitrate } else { bitrate } as u64;
        arb * SEC / bitrate as u64 + data_bits * SEC / dr
    } else {
        let dl = if f.id & CAN_RTR_FLAG != 0 { 0 } else { f.len as u64 };
        // frame bits + interframe space, ~10 % stuffing
        let bits = (if ext { 67 } else { 47 } + 8 * dl) * 11 / 10 + 3;
        bits * SEC / bitrate as u64
    }
}

impl Recorder {
    pub fn new(cfg: &Config, shared: Arc<Shared>, pipe: Arc<Pipeline>, cmds: Receiver<Request>) -> Recorder {
        let now = boot_ns();
        let mut r = Recorder {
            cfg: cfg.clone(),
            shared: shared.clone(),
            pipe,
            cmds,
            sock: None,
            tx_sock: None,
            stream_buf: Vec::new(),
            sock_error: None,
            sim: None,
            ifaces: IfaceTable::default(),
            cur: None,
            builder: BlockBuilder::new(),
            block_opened: now,
            flush_ns: cfg.u64_or("flush_ms", 1000).clamp(50, 2000) * MS,
            session_ord: 0,
            session_start_flag: false,
            session_started_at: now,
            name: cfg.str_or("session_name", ""),
            time: None,
            set_clock: cfg.bool_or("set_system_clock", true),
            dropped_total: 0,
            marks: 0,
            live: HashMap::new(),
            frames_total: 0,
            frames_prev: 0,
            fps: 0.0,
            written_prev: 0,
            write_bps: 0.0,
            pf_prev: false,
            rt_off: realtime_ns() - now as i64,
            t_checkpoint: now,
            t_scan: 0,
            t_publish: 0,
            t_sock_retry: 0,
            frames: Vec::with_capacity(4096),
        };
        // wall clock already valid (Pi 5 RTC with battery, or a restart after a sync)
        let rt = realtime_ns();
        if rt > VALID_TIME_NS {
            let src = if std::path::Path::new("/dev/rtc0").exists() { time_source::RTC } else { time_source::MANUAL };
            r.time = Some((rt - now as i64, src));
        }
        if shared.sim {
            r.sim = Some(Sim::new(now));
            for (ifindex, name, bitrate, dbitrate) in [(sim::SIM0, "sim0", 500_000, 0), (sim::SIM1, "sim1", 500_000, 2_000_000)] {
                let info = IfaceInfo {
                    name: name.into(),
                    label: if ifindex == sim::SIM0 { "Powertrain (simulated)" } else { "Body (simulated)" }.into(),
                    serial: format!("SIM{}", -ifindex),
                    usb_port: String::new(),
                    driver: "sim".into(),
                    bitrate,
                    dbitrate,
                    fd: dbitrate > 0,
                    listen_only: true,
                };
                r.ifaces.add_sim(ifindex, info);
            }
        } else {
            r.open_socket();
        }
        r
    }

    fn open_socket(&mut self) {
        match CanSocket::open(self.cfg.u64_or("socket_rcvbuf_kb", 8192) as usize * 1024) {
            Ok(s) => {
                self.sock = Some(s);
                self.sock_error = None;
            }
            Err(e) => {
                if self.sock_error.is_none() {
                    eprintln!("canlogd: cannot open CAN socket: {e}");
                }
                self.sock_error = Some(e.to_string());
            }
        }
    }

    // ------------------------------------------------------------ blocks

    fn push(&mut self, typ: u8, iface: u8, flags: u8, ts: u64, parts: &[&[u8]]) {
        for _ in 0..2 {
            if self.cur.is_none() {
                self.cur = Some(self.pipe.get_buf(&self.shared));
                self.block_opened = boot_ns();
            }
            let buf = self.cur.as_mut().unwrap();
            if self.builder.push(buf, typ, iface, flags, ts, parts) {
                return;
            }
            self.finish_block(false);
        }
    }

    fn finish_block(&mut self, pf: bool) {
        if self.builder.is_empty() {
            return;
        }
        let Some(mut buf) = self.cur.take() else { return };
        let mut flags = 0;
        if pf {
            flags |= F_POWER_FAIL;
        }
        if self.session_start_flag {
            flags |= F_SESSION_START;
            self.session_start_flag = false;
        }
        let (off, src) = self.time.unwrap_or((0, 0));
        if self.time.is_some() {
            flags |= F_TIME_VALID;
            if src == time_source::RTC {
                flags |= F_TIME_FROM_RTC;
            }
        }
        let hdr = BlockHeader {
            session: self.session_ord,
            flags,
            utc_offset_ns: off,
            dropped: self.dropped_total,
            name: truncate_utf8(&self.name, NAME_MAX).to_string(),
            ..Default::default()
        };
        self.builder.finish(&mut buf, hdr);
        self.pipe.submit(buf, pf);
    }

    fn push_checkpoint(&mut self, now: u64) {
        self.finish_block(false);
        for i in 0..self.ifaces.list.len() {
            let (idx, enc) = (self.ifaces.list[i].idx, self.ifaces.list[i].info.encode());
            self.push(rt::IFACE, idx, 0, now, &[&enc]);
        }
        if let Some((off, src)) = self.time {
            self.push(rt::TIMESYNC, 0, 0, now, &[&off.to_le_bytes(), &[src]]);
        }
        if !self.name.is_empty() {
            let n = truncate_utf8(&self.name, 200).to_string();
            self.push(rt::NAME, 0, 0, now, &[n.as_bytes()]);
        }
        self.builder.flags |= F_CHECKPOINT;
        self.t_checkpoint = now;
    }

    fn start_session(&mut self, now: u64) {
        self.finish_block(false);
        self.session_start_flag = true;
        self.session_started_at = now;
        let v = format!("canlogd {}", env!("CARGO_PKG_VERSION"));
        self.push(rt::SESSION, 0, 0, now, &[v.as_bytes()]);
        self.push_checkpoint(now);
    }

    // ------------------------------------------------------------ frames

    fn handle_frames(&mut self, now: u64) {
        let frames = std::mem::take(&mut self.frames);
        let mut rescanned = false;
        let streaming = self.shared.stream_count.load(Relaxed) > 0;
        for f in &frames {
            let idx = match self.ifaces.get_idx(f.ifindex) {
                Some(i) => i,
                None if !rescanned => {
                    rescanned = true;
                    self.scan(now);
                    self.ifaces.get_idx(f.ifindex).unwrap_or(255)
                }
                None => 255,
            };
            let ts = match f.ts_rt_ns {
                Some(rt) => {
                    let t = rt - self.rt_off;
                    if t <= 0 || t as u64 > now + SEC || (t as u64) + 60 * SEC < now {
                        now
                    } else {
                        t as u64
                    }
                }
                None => now,
            };
            let dl = if !f.fd && f.id & CAN_RTR_FLAG != 0 { 0 } else { f.len as usize };
            let typ = if f.fd { rt::CANFD } else { rt::CAN };
            let rflags = f.flags | if f.local { CAN_TX_LOCAL } else { 0 };
            self.push(typ, idx, rflags, ts, &[&f.id.to_le_bytes(), &[f.len], &f.data[..dl]]);
            self.frames_total += 1;
            if streaming {
                self.stream_record(f, idx, ts, dl);
            }

            let mut state_change = None;
            if let Some(it) = self.ifaces.by_idx_mut(idx) {
                it.frames += 1;
                let busy = bits_ns(f, it.info.bitrate, it.info.dbitrate);
                it.busy_ns += busy;
                // 100 ms windows for the peak load
                if ts >= it.win_start + 100 * MS {
                    it.peak = it.peak.max(it.win_busy as f64 / (100 * MS) as f64 * 100.0);
                    it.win_start = ts - ts % (100 * MS);
                    it.win_busy = 0;
                }
                it.win_busy += busy;
                if f.id & CAN_ERR_FLAG != 0 {
                    it.errors += 1;
                    let mut st = None;
                    if f.id & 0x40 != 0 {
                        st = Some(ifstate::BUS_OFF);
                    } else if f.id & 0x04 != 0 {
                        let d1 = f.data[1];
                        if d1 & 0x30 != 0 {
                            st = Some(ifstate::ERROR_PASSIVE);
                        } else if d1 & 0x0C != 0 {
                            st = Some(ifstate::ERROR_WARNING);
                        } else if d1 & 0x40 != 0 {
                            st = Some(ifstate::ERROR_ACTIVE);
                        }
                    }
                    if f.id & 0x100 != 0 {
                        st = Some(ifstate::ERROR_ACTIVE);
                    }
                    if let Some(s) = st {
                        if s != it.state {
                            it.state = s;
                            state_change = Some(s);
                        }
                    }
                } else if it.state != ifstate::UP && it.state != ifstate::ERROR_ACTIVE && it.state != ifstate::ERROR_WARNING {
                    // frames arriving again after bus-off / passive
                    it.state = ifstate::ERROR_ACTIVE;
                    state_change = Some(ifstate::ERROR_ACTIVE);
                }
            }
            if let Some(s) = state_change {
                self.push(rt::IFSTATE, idx, 0, ts, &[&[s]]);
            }
            if f.id & CAN_ERR_FLAG == 0 {
                let key = (idx, f.id & !CAN_RTR_FLAG);
                if self.live.len() < LIVE_MAX || self.live.contains_key(&key) {
                    let e = self.live.entry(key).or_insert(Live { fd: f.fd, len: 0, data: [0; 64], count: 0, count_prev: 0, hz: 0.0, last_ts: 0 });
                    e.fd = f.fd;
                    e.len = f.len;
                    e.data[..dl].copy_from_slice(&f.data[..dl]);
                    e.count += 1;
                    e.last_ts = ts;
                }
            }
        }
        self.frames = frames;
        self.frames.clear();
        if streaming && !self.stream_buf.is_empty() {
            self.flush_stream();
        }
    }

    fn stream_record(&mut self, f: &RxFrame, idx: u8, ts: u64, dl: usize) {
        use proto::*;
        let mut fl = 0u8;
        let id = if f.id & CAN_ERR_FLAG != 0 {
            fl |= STREAM_F_ERR;
            f.id & CAN_EFF_MASK
        } else if f.id & CAN_EFF_FLAG != 0 {
            fl |= STREAM_F_EXT;
            f.id & CAN_EFF_MASK
        } else {
            f.id & CAN_SFF_MASK
        };
        if f.id & CAN_RTR_FLAG != 0 {
            fl |= STREAM_F_RTR;
        }
        if f.fd {
            fl |= STREAM_F_FD;
            if f.flags & CANFD_BRS != 0 {
                fl |= STREAM_F_BRS;
            }
            if f.flags & CANFD_ESI != 0 {
                fl |= STREAM_F_ESI;
            }
        }
        if f.local {
            fl |= STREAM_F_TX;
        }
        let t = match self.time {
            Some((off, _)) => ts as i64 + off,
            None => ts as i64,
        };
        let b = &mut self.stream_buf;
        b.extend_from_slice(&t.to_le_bytes());
        b.extend_from_slice(&id.to_le_bytes());
        b.extend_from_slice(&[idx, fl, dl as u8, 0]);
        b.extend_from_slice(&f.data[..dl]);
    }

    /// Hand the batch to every stream client; a full client queue loses the
    /// batch, a closed one is removed.
    fn flush_stream(&mut self) {
        let batch = Arc::new(std::mem::take(&mut self.stream_buf));
        self.shared.streams.lock().unwrap().retain(|s| !matches!(s.try_send(batch.clone()), Err(std::sync::mpsc::TrySendError::Disconnected(_))));
    }

    fn scan(&mut self, now: u64) {
        if self.sim.is_some() {
            return;
        }
        for ev in self.ifaces.scan(&self.cfg) {
            match ev {
                Event::Added(i) => {
                    let (idx, enc) = (self.ifaces.list[i].idx, self.ifaces.list[i].info.encode());
                    self.push(rt::IFACE, idx, 0, now, &[&enc]);
                    self.push(rt::IFSTATE, idx, 0, now, &[&[ifstate::UP]]);
                }
                Event::Removed(i) => {
                    let idx = self.ifaces.list[i].idx;
                    self.push(rt::IFSTATE, idx, 0, now, &[&[ifstate::REMOVED]]);
                }
            }
        }
    }

    // ------------------------------------------------------------ commands

    fn handle_req(&mut self, req: Request, now: u64) {
        let res = match req.cmd {
            Command::Send { iface, id, ext, fd, brs, data } => self.send(&iface, id, ext, fd, brs, &data, now),
            Command::TxMode { iface, on } => self.tx_mode(&iface, on, now),
            cmd => {
                self.handle_cmd(cmd, now);
                proto::ok()
            }
        };
        if let Some(r) = req.reply {
            let _ = r.send(res);
        }
    }

    fn log_iface(&mut self, i: usize, now: u64, text: String) {
        let (idx, enc) = (self.ifaces.list[i].idx, self.ifaces.list[i].info.encode());
        self.push(rt::IFACE, idx, 0, now, &[&enc]);
        self.push(rt::MARK, 0, 0, now, &[text.as_bytes()]);
    }

    fn tx_mode(&mut self, iface: &str, on: bool, now: u64) -> Value {
        let Some(i) = self.ifaces.find(iface) else { return err(format!("no interface {iface}")) };
        if on && !self.ifaces.list[i].tx_allowed {
            return err(format!("sending is not enabled on {iface} (logger.conf: can.<serial, port or name>.tx = 1)"));
        }
        if self.ifaces.list[i].tx_mode == on {
            return proto::ok();
        }
        if let Err(e) = self.ifaces.set_tx_mode(&self.cfg, i, on) {
            return err(e);
        }
        let name = self.ifaces.list[i].info.name.clone();
        let text = if on { format!("{name}: TX mode on (listen-only off, frames are ACKed)") } else { format!("{name}: back to listen-only") };
        self.log_iface(i, now, text);
        proto::ok()
    }

    #[allow(clippy::too_many_arguments)]
    fn send(&mut self, iface: &str, id: u32, ext: bool, fd: bool, brs: bool, data: &str, now: u64) -> Value {
        let Some(bytes) = proto::parse_hex(data) else { return err("data must be hex bytes") };
        if (!fd && bytes.len() > 8) || bytes.len() > 64 || (fd && bytes.len() > 8 && !matches!(bytes.len(), 12 | 16 | 20 | 24 | 32 | 48 | 64)) {
            return err(format!("invalid payload length {}", bytes.len()));
        }
        if (ext && id > CAN_EFF_MASK) || (!ext && id > CAN_SFF_MASK) {
            return err("id out of range");
        }
        let Some(i) = self.ifaces.find(iface) else { return err(format!("no interface {iface}")) };
        let it = &self.ifaces.list[i];
        if fd && !it.info.fd {
            return err(format!("{iface} is not configured for CAN FD (dbitrate)"));
        }
        let mut changed = false;
        if !it.tx_mode {
            let r = self.tx_mode(iface, true, now);
            if r["ok"] != true {
                return r;
            }
            changed = true;
        }
        let it = &self.ifaces.list[i];
        let raw_id = if ext { id | CAN_EFF_FLAG } else { id };
        if it.ifindex < 0 {
            // simulator: the frame goes straight into the recording
            let mut f = RxFrame { ifindex: it.ifindex, ts_rt_ns: None, id: raw_id, fd, flags: if fd && brs { CANFD_BRS } else { 0 }, len: bytes.len() as u8, data: [0; 64], local: true };
            f.data[..bytes.len()].copy_from_slice(&bytes);
            self.frames.push(f);
            self.handle_frames(now);
        } else {
            if self.tx_sock.is_none() {
                match TxSocket::open() {
                    Ok(s) => self.tx_sock = Some(s),
                    Err(e) => return err(format!("TX socket: {e}")),
                }
            }
            if let Err(e) = self.tx_sock.as_ref().unwrap().send(it.ifindex, raw_id, fd, brs, &bytes) {
                return err(format!("send failed: {e}"));
            }
        }
        json!({"ok": true, "tx_mode_changed": changed})
    }

    fn handle_cmd(&mut self, cmd: Command, now: u64) {
        match cmd {
            Command::Timesync { utc_ms, source } => {
                let src = time_source::parse(source.as_deref().unwrap_or(""));
                let off = utc_ms as i64 * 1_000_000 - now as i64;
                // ignore obviously wrong clocks
                if (utc_ms as i64) * 1_000_000 < VALID_TIME_NS {
                    return;
                }
                self.time = Some((off, src));
                self.push(rt::TIMESYNC, 0, 0, now, &[&off.to_le_bytes(), &[src]]);
                if self.set_clock && !self.shared.sim {
                    let rt = realtime_ns();
                    let target = utc_ms as i64 * 1_000_000;
                    if (rt - target).abs() > SEC as i64 {
                        let ts = libc::timespec { tv_sec: (target / 1_000_000_000) as _, tv_nsec: (target % 1_000_000_000) as _ };
                        if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) } == 0 {
                            eprintln!("canlogd: system clock set to {}", canlog_core::timefmt::iso(utc_ms));
                            self.rt_off = realtime_ns() - boot_ns() as i64;
                            if std::path::Path::new("/dev/rtc0").exists() {
                                // keep the Pi 5 RTC in sync for the next boot
                                std::thread::spawn(|| {
                                    let _ = std::process::Command::new("hwclock").args(["-w", "-u"]).status();
                                });
                            }
                        }
                    }
                }
            }
            Command::Name { name } => {
                self.name = name.trim().to_string();
                let n = truncate_utf8(&self.name, 200).to_string();
                self.push(rt::NAME, 0, 0, now, &[n.as_bytes()]);
            }
            Command::Mark { text } => {
                let t = truncate_utf8(text.trim(), 250).to_string();
                self.marks += 1;
                self.push(rt::MARK, 0, 0, now, &[t.as_bytes()]);
            }
            Command::NewSession { name } => {
                self.finish_block(false);
                self.session_ord += 1;
                self.name = name.unwrap_or_default().trim().to_string();
                self.dropped_total = 0;
                self.marks = 0;
                self.live.clear();
                for it in &mut self.ifaces.list {
                    it.frames = 0;
                    it.errors = 0;
                    it.frames_prev = 0;
                    it.peak_max = 0.0;
                }
                self.frames_prev = self.frames_total;
                self.start_session(now);
            }
            Command::SimPowerfail { on } => {
                if on && !self.shared.pf_asserted.load(Relaxed) {
                    self.shared.pf_events.fetch_add(1, Relaxed);
                }
                self.shared.pf_asserted.store(on, Relaxed);
            }
            Command::Status | Command::Live { .. } | Command::Subscribe | Command::Stream | Command::Send { .. } | Command::TxMode { .. } => {}
        }
    }

    // ------------------------------------------------------------ status

    fn publish(&mut self, now: u64, dt_s: f64) {
        let fr = self.frames_total;
        self.fps = (fr - self.frames_prev) as f64 / dt_s;
        self.frames_prev = fr;
        let w = self.shared.written_blocks.load(Relaxed);
        let bps = (w - self.written_prev) as f64 * BLOCK_SIZE as f64 / dt_s;
        self.written_prev = w;
        self.write_bps = if self.write_bps == 0.0 { bps } else { self.write_bps * 0.9 + bps * 0.1 };

        let mut ifs = Vec::new();
        for it in &mut self.ifaces.list {
            it.fps = (it.frames - it.frames_prev) as f64 / dt_s;
            it.frames_prev = it.frames;
            it.load = (it.busy_ns as f64 / (dt_s * 1e9) * 100.0).min(100.0);
            it.busy_ns = 0;
            it.peak_shown = it.peak.min(100.0).max(it.load);
            it.peak_max = it.peak_max.max(it.peak_shown);
            it.peak = 0.0;
            ifs.push(json!({
                "idx": it.idx,
                "name": it.info.name,
                "label": it.info.label,
                "serial": it.info.serial,
                "usb_port": it.info.usb_port,
                "driver": it.info.driver,
                "bitrate": it.info.bitrate,
                "dbitrate": it.info.dbitrate,
                "fd": it.info.fd,
                "listen_only": it.info.listen_only,
                "present": it.present,
                "state": ifaces::state_name(it.state),
                "fps": (it.fps * 10.0).round() / 10.0,
                "load_pct": (it.load * 10.0).round() / 10.0,
                "load_peak_pct": (it.peak_shown * 10.0).round() / 10.0,
                "load_max_pct": (it.peak_max * 10.0).round() / 10.0,
                "tx_allowed": it.tx_allowed,
                "tx_mode": it.tx_mode,
                "frames": it.frames,
                "errors": it.errors,
                "config_error": it.config_error,
            }));
        }

        // live table
        let names: HashMap<u8, String> = self.ifaces.list.iter().map(|i| (i.idx, i.info.name.clone())).collect();
        let mut keys: Vec<_> = self.live.keys().copied().collect();
        keys.sort_unstable_by_key(|(i, id)| (*i, id & CAN_EFF_MASK, *id));
        let mut live = Vec::with_capacity(keys.len().min(1024));
        for k in keys.into_iter().take(1024) {
            let e = self.live.get_mut(&k).unwrap();
            e.hz = (e.count - e.count_prev) as f64 / dt_s;
            e.count_prev = e.count;
            let ext = k.1 & CAN_EFF_FLAG != 0;
            let id = if ext { format!("{:08X}", k.1 & CAN_EFF_MASK) } else { format!("{:03X}", k.1 & CAN_SFF_MASK) };
            let rtr = k.1 & CAN_RTR_FLAG != 0;
            let dl = if rtr { 0 } else { e.len as usize };
            let data: String = e.data[..dl].iter().map(|b| format!("{b:02X}")).collect();
            live.push(json!({
                "if": names.get(&k.0).cloned().unwrap_or_else(|| "?".into()),
                "id": id,
                "ext": ext,
                "fd": e.fd,
                "len": e.len,
                "data": data,
                "hz": (e.hz * 10.0).round() / 10.0,
                "n": e.count,
                "age_ms": now.saturating_sub(e.last_ts) / MS,
            }));
        }
        *self.shared.live.lock().unwrap() = live;

        let sh = &self.shared;
        let n = sh.ring_blocks.load(Relaxed);
        let head = sh.head_seq.load(Relaxed);
        let base = sh.session_base.load(Relaxed);
        let cap_bytes = n as f64 * BLOCK_SIZE as f64;
        let utc_now = self.time.map(|(o, _)| (now as i64 + o) / 1_000_000);
        let status = json!({
            "v": 1,
            "uptime_s": now / SEC,
            "state": writer::state_name(sh.state.load(Relaxed)),
            "sim": sh.sim,
            "session": {
                "id": if base > 0 { Some(base + self.session_ord) } else { None },
                "name": self.name,
                "duration_s": (now - self.session_started_at) / SEC,
                "start_utc_ms": self.time.map(|(o, _)| (self.session_started_at as i64 + o) / 1_000_000),
                "marks": self.marks,
            },
            "time": {
                "synced": self.time.is_some(),
                "source": self.time.map(|(_, s)| time_source::name(s)),
                "utc_ms": utc_now,
            },
            "rec": {
                "fps": (self.fps * 10.0).round() / 10.0,
                "frames": self.frames_total,
                "dropped_frames": self.dropped_total,
                "queue_blocks": self.pipe.queued(),
                "written_blocks": sh.written_blocks.load(Relaxed),
                "write_errors": sh.write_errors.load(Relaxed),
                "lost_blocks": sh.lost_blocks.load(Relaxed),
                "write_kbps": (self.write_bps / 1024.0 * 10.0).round() / 10.0,
            },
            "ring": {
                "size_mb": (cap_bytes / 1048576.0).round(),
                "head_seq": head,
                "used_pct": if n > 0 { ((head.min(n) as f64 / n as f64) * 1000.0).round() / 10.0 } else { 0.0 },
                "wrapped": n > 0 && head > n,
                "hours_at_current_rate": if self.write_bps > 1.0 { Some((cap_bytes / self.write_bps / 3600.0 * 10.0).round() / 10.0) } else { None },
            },
            "power": {
                "fail": sh.pf_asserted.load(Relaxed),
                "events": sh.pf_events.load(Relaxed),
            },
            "can_socket_error": self.sock_error,
            "ifaces": ifs,
        });
        *self.shared.status_json.lock().unwrap() = status.to_string();
    }

    // ------------------------------------------------------------ main loop

    pub fn run(&mut self, signalled: impl Fn() -> bool) {
        let now = boot_ns();
        self.scan(now);
        self.start_session(now);
        self.publish(now, 1.0);
        self.t_publish = now;
        let mut last_publish = now;

        loop {
            let now = boot_ns();
            self.shared.rx_heartbeat.store(now, Relaxed);
            if signalled() {
                break;
            }

            // wait for frames / wake-ups, at most until the next deadline
            let mut next = now + 100 * MS;
            if !self.builder.is_empty() {
                next = next.min(self.block_opened + self.flush_ns);
            }
            if self.sim.is_some() {
                next = next.min(now + 10 * MS);
            }
            let timeout_ms = (next.saturating_sub(now) / MS) as i32;
            let mut pfds = [
                libc::pollfd { fd: self.shared.wake_fd, events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: self.sock.as_ref().map(|s| s.fd).unwrap_or(-1), events: libc::POLLIN, revents: 0 },
            ];
            unsafe { libc::poll(pfds.as_mut_ptr(), 2, timeout_ms) };
            if pfds[0].revents & libc::POLLIN != 0 {
                let mut v = 0u64;
                unsafe { libc::read(self.shared.wake_fd, &mut v as *mut u64 as *mut libc::c_void, 8) };
            }

            let now = boot_ns();
            self.rt_off = realtime_ns() - now as i64;

            // frames
            if let Some(sim) = self.sim.as_mut() {
                sim.poll(now, self.rt_off, &mut self.frames);
                self.handle_frames(now);
            } else if let Some(mut sock) = self.sock.take() {
                for _ in 0..64 {
                    match sock.recv_batch(&mut self.frames) {
                        Ok(d) => {
                            if d > 0 {
                                self.dropped_total = self.dropped_total.saturating_add(d);
                                self.push(rt::DROPPED, 0, 0, now, &[&d.to_le_bytes()]);
                            }
                        }
                        Err(e) => {
                            eprintln!("canlogd: receive error: {e}");
                            break;
                        }
                    }
                    if self.frames.is_empty() {
                        break;
                    }
                    self.handle_frames(now);
                }
                self.sock = Some(sock);
            } else if now >= self.t_sock_retry {
                self.t_sock_retry = now + 5 * SEC;
                self.open_socket();
            }

            // commands from clients
            while let Ok(req) = self.cmds.try_recv() {
                self.handle_req(req, now);
            }

            // power-fail transitions
            let pf = self.shared.pf_asserted.load(Relaxed);
            if pf != self.pf_prev {
                self.pf_prev = pf;
                self.push(rt::POWER, 0, 0, now, &[&[pf as u8]]);
                if pf {
                    eprintln!("canlogd: power fail: flushing and holding writes");
                    self.finish_block(true);
                } else {
                    eprintln!("canlogd: power restored");
                }
                self.pipe.notify();
            }

            // housekeeping
            if now >= self.t_scan + 250 * MS {
                self.t_scan = now;
                self.scan(now);
            }
            if now >= self.t_checkpoint + CHECKPOINT_NS {
                self.push_checkpoint(now);
            }
            if !self.builder.is_empty() && now >= self.block_opened + self.flush_ns {
                self.finish_block(false);
            }
            if now >= last_publish + SEC {
                let dt = (now - last_publish) as f64 / 1e9;
                last_publish = now;
                self.publish(now, dt);
            }
        }

        // shutdown: flush what we have
        let now = boot_ns();
        if let Some(sim) = self.sim.as_mut() {
            sim.poll(now, self.rt_off, &mut self.frames);
            self.handle_frames(now);
        }
        self.finish_block(false);
    }
}
