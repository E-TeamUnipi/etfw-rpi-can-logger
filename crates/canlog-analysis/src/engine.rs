//! Everything the web app works with: one dataset (an imported log or the
//! live feed), the DBCs assigned to each bus, and cached signal series.

use crate::dataset::*;
use crate::logfile::LogReader;
use crate::rta::{self, RtaMsg, RtaOptions, RtaReport};
use crate::stats::{self, BusLoad, MsgStats};
use canlog_dbc::{decode_message, raw_to_phys, signal_active, Dbc, DecodedSignal, Message};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Default)]
struct Series {
    generation: u32,
    /// frames of the message already decoded
    upto: usize,
    t: Vec<f64>,
    v: Vec<f64>,
}

#[derive(Default)]
pub struct Engine {
    pub ds: Dataset,
    dbcs: HashMap<String, Dbc>,
    bus_dbcs: HashMap<String, Vec<String>>,
    reader: Option<LogReader>,
    series: HashMap<(u16, u32, String), Series>,
    /// Live: dataset index per logger interface index.
    live_bus: Vec<u16>,
    pub live_cap: usize,
}

#[derive(Serialize)]
pub struct DbcSummary {
    pub version: Option<String>,
    pub messages: usize,
    pub signals: usize,
    pub bitrate: Option<u32>,
    pub data_bitrate: Option<u32>,
    pub warnings: Vec<String>,
    pub fd: bool,
}

#[derive(Serialize)]
pub struct ImportSummary {
    pub format: String,
    pub frames: usize,
    pub buses: Vec<String>,
    pub lines: u64,
    pub bad_lines: u64,
    pub first_error: Option<String>,
    pub start_s: f64,
    pub end_s: f64,
    pub wall_clock: bool,
    pub markers: Vec<(f64, String)>,
    pub memory_mb: f64,
}

#[derive(Serialize)]
pub struct MsgRow {
    pub id: u32,
    pub ext: bool,
    pub name: Option<String>,
    pub count: u64,
    pub data: Vec<u8>,
    pub t: f64,
    pub hz: f64,
    pub signals: Vec<DecodedSignal>,
}

#[derive(Serialize)]
pub struct SignalInfo {
    pub name: String,
    pub unit: String,
    pub min: f64,
    pub max: f64,
    pub factor: f64,
    pub offset: f64,
    pub size: u16,
    pub signed: bool,
    pub float: bool,
    pub mux: Option<String>,
    pub choices: Vec<(i64, String)>,
    pub comment: String,
    pub start_raw: Option<f64>,
}

#[derive(Serialize)]
pub struct MsgInfo {
    pub dbc: String,
    pub name: String,
    pub id: u32,
    pub ext: bool,
    pub size: u8,
    pub fd: bool,
    pub brs: bool,
    pub cycle_ms: Option<f64>,
    pub send_type: Option<String>,
    pub sender: String,
    pub comment: String,
    pub signals: Vec<SignalInfo>,
}

/// Per-message overrides from the app (e.g. minimum gap of event messages).
#[derive(Deserialize, Default)]
pub struct RtaOverride {
    pub id: u32,
    pub ext: bool,
    pub period_ms: Option<f64>,
    #[serde(default)]
    pub jitter_ms: Option<f64>,
    #[serde(default)]
    pub deadline_ms: Option<f64>,
}

impl Engine {
    pub fn new() -> Engine {
        Engine { live_cap: 4_000_000, ..Default::default() }
    }

    // ---------------------------------------------------------------- DBCs

    pub fn load_dbc(&mut self, key: &str, bytes: &[u8]) -> DbcSummary {
        let dbc = canlog_dbc::parse(&canlog_dbc::text_from_bytes(bytes));
        let s = DbcSummary {
            version: dbc.version.clone(),
            messages: dbc.messages.len(),
            signals: dbc.messages.iter().map(|m| m.signals.len()).sum(),
            bitrate: dbc.bitrate,
            data_bitrate: dbc.data_bitrate,
            warnings: dbc.warnings.clone(),
            fd: dbc.messages.iter().any(|m| m.fd),
        };
        self.dbcs.insert(key.to_string(), dbc);
        self.series.clear();
        s
    }

    pub fn remove_dbc(&mut self, key: &str) {
        self.dbcs.remove(key);
        self.series.clear();
    }

    /// DBCs (by key) used to decode a bus, in priority order.
    pub fn assign(&mut self, bus: &str, keys: Vec<String>) {
        self.bus_dbcs.insert(bus.to_string(), keys);
        self.series.clear();
    }

    fn dbcs_of(&self, bus: &str) -> impl Iterator<Item = (&String, &Dbc)> {
        self.bus_dbcs.get(bus).into_iter().flatten().filter_map(|k| self.dbcs.get_key_value(k))
    }

    pub fn message(&self, bus: &str, id: u32, ext: bool) -> Option<&Message> {
        self.dbcs_of(bus).find_map(|(_, d)| d.message(id, ext))
    }

    pub fn message_by_name(&self, bus: &str, name: &str) -> Option<&Message> {
        self.dbcs_of(bus).find_map(|(_, d)| d.message_by_name(name))
    }

    pub fn messages(&self, bus: &str) -> Vec<MsgInfo> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (k, d) in self.dbcs_of(bus) {
            for m in &d.messages {
                if !seen.insert((m.id, m.extended)) {
                    continue;
                }
                out.push(MsgInfo {
                    dbc: k.clone(),
                    name: m.name.clone(),
                    id: m.id,
                    ext: m.extended,
                    size: m.size,
                    fd: m.fd,
                    brs: m.brs,
                    cycle_ms: m.cycle_ms,
                    send_type: m.send_type.clone(),
                    sender: m.sender.clone(),
                    comment: m.comment.clone(),
                    signals: m
                        .signals
                        .iter()
                        .map(|s| SignalInfo {
                            name: s.name.clone(),
                            unit: s.unit.clone(),
                            min: s.min,
                            max: s.max,
                            factor: s.factor,
                            offset: s.offset,
                            size: s.size,
                            signed: s.signed,
                            float: s.value_type != canlog_dbc::ValueType::Int,
                            mux: match &s.mux {
                                canlog_dbc::Mux::None => None,
                                canlog_dbc::Mux::Multiplexor => Some("M".into()),
                                canlog_dbc::Mux::Multiplexed { value, also_multiplexor } => {
                                    Some(format!("m{value}{}", if *also_multiplexor { "M" } else { "" }))
                                }
                            },
                            choices: s.choices.clone(),
                            comment: s.comment.clone(),
                            start_raw: s.start_raw,
                        })
                        .collect(),
                });
            }
        }
        out.sort_by_key(|m| (m.ext, m.id));
        out
    }

    pub fn encode(&self, bus: &str, msg: &str, values: &[(String, f64)]) -> Option<Vec<u8>> {
        let m = self.message_by_name(bus, msg)?;
        Some(canlog_dbc::encode_message(m, values))
    }

    // ---------------------------------------------------------------- data in

    pub fn clear(&mut self) {
        self.ds.clear();
        self.series.clear();
        self.reader = None;
    }

    pub fn import_begin(&mut self) {
        self.ds = self.ds.next(false);
        self.series.clear();
        self.reader = Some(LogReader::default());
    }

    pub fn import_push(&mut self, chunk: &[u8]) {
        if let Some(r) = self.reader.as_mut() {
            r.push(chunk, &mut self.ds);
        }
    }

    pub fn import_end(&mut self) -> ImportSummary {
        let mut r = self.reader.take().unwrap_or_default();
        r.finish(&mut self.ds);
        self.summary(r.format_name(), r.lines, r.bad_lines, r.first_error.clone())
    }

    pub fn summary(&self, format: &str, lines: u64, bad: u64, err: Option<String>) -> ImportSummary {
        let (a, b) = self.ds.time_range().unwrap_or((0, 0));
        let t = |x| stats::time_s(&self.ds, x, a);
        ImportSummary {
            format: format.to_string(),
            frames: self.ds.len(),
            buses: self.ds.bus_names.clone(),
            lines,
            bad_lines: bad,
            first_error: err,
            start_s: t(a),
            end_s: t(b),
            wall_clock: self.ds.wall_clock,
            markers: self.ds.markers.iter().map(|(ts, s)| (t(*ts), s.clone())).collect(),
            memory_mb: self.ds.memory() as f64 / 1e6,
        }
    }

    /// Start a live dataset. `buses` maps logger interface index -> bus name.
    pub fn live_begin(&mut self, buses: &[String]) {
        self.ds = self.ds.next(true);
        self.series.clear();
        self.live_bus = buses.iter().map(|b| self.ds.bus_index(b)).collect();
    }

    pub fn live_set_buses(&mut self, buses: &[String]) {
        self.live_bus = buses.iter().map(|b| self.ds.bus_index(b)).collect();
    }

    /// Binary records from the logger's stream (see canweb `/api/stream`):
    /// `ts_ns i64, id u32, iface u8, flags u8, len u8, pad u8, data[len]`,
    /// little endian. Returns bytes consumed (a trailing partial record is
    /// left for the next call).
    pub fn live_push(&mut self, buf: &[u8]) -> usize {
        let mut p = 0;
        while p + 16 <= buf.len() {
            let len = buf[p + 14] as usize;
            if p + 16 + len > buf.len() {
                break;
            }
            let ts = i64::from_le_bytes(buf[p..p + 8].try_into().unwrap());
            let id = u32::from_le_bytes(buf[p + 8..p + 12].try_into().unwrap());
            let iface = buf[p + 12] as usize;
            let flags = buf[p + 13];
            let bus = match self.live_bus.get(iface) {
                Some(&b) => b,
                None => self.ds.bus_index(&format!("can{iface}")),
            };
            self.ds.push(ts, bus, id, flags, &buf[p + 16..p + 16 + len]);
            p += 16 + len;
        }
        self.cap();
        p
    }

    /// One frame (BLE snapshots, frames we sent).
    pub fn push_frame(&mut self, bus: &str, ts_ns: i64, id: u32, flags: u8, data: &[u8]) {
        let b = self.ds.bus_index(bus);
        self.ds.push(ts_ns, b, id, flags, data);
        self.cap();
    }

    fn cap(&mut self) {
        if self.live_cap > 0 && self.ds.len() > self.live_cap {
            self.ds.drop_front(self.live_cap / 2);
            self.series.clear();
        }
    }

    // ---------------------------------------------------------------- views

    pub fn bus_load(&self, bitrates: &HashMap<String, (u32, u32)>) -> Vec<BusLoad> {
        let br: Vec<(u32, u32)> = self.ds.bus_names.iter().map(|b| bitrates.get(b).copied().unwrap_or((0, 0))).collect();
        stats::bus_load(&self.ds, &br)
    }

    pub fn msg_stats(&self, bus: &str, bitrate: u32, dbitrate: u32) -> Vec<(MsgStats, Option<String>)> {
        let Some(b) = self.ds.bus_names.iter().position(|n| n == bus) else { return Vec::new() };
        stats::msg_stats(&self.ds, b as u16, bitrate, dbitrate)
            .into_iter()
            .map(|s| {
                let name = self.message(bus, s.id, s.ext).map(|m| m.name.clone());
                (s, name)
            })
            .collect()
    }

    /// Latest frame of every message on a bus, decoded.
    pub fn latest(&self, bus: &str) -> Vec<MsgRow> {
        let Some(b) = self.ds.bus_names.iter().position(|n| n == bus) else { return Vec::new() };
        let b = b as u16;
        let t0 = self.ds.ts.first().copied().unwrap_or(0);
        let mut keys: Vec<u32> = self.ds.keys().filter(|k| k.0 == b).map(|k| k.1).collect();
        keys.sort_unstable_by_key(|k| (k & 0x8000_0000 != 0, k & 0x1FFF_FFFF));
        keys.into_iter()
            .map(|key| {
                let idx = self.ds.frames_of(b, key);
                let f = self.ds.frame(*idx.last().unwrap() as usize);
                let (id, ext) = (key & 0x1FFF_FFFF, key & 0x8000_0000 != 0);
                let m = self.message(bus, id, ext);
                // rate over the last (up to) 20 frames
                let hz = if idx.len() > 1 {
                    let k = idx.len().min(20);
                    let first = self.ds.ts[idx[idx.len() - k] as usize];
                    let dt = (f.ts_ns - first) as f64 / 1e9;
                    if dt > 0.0 { (k - 1) as f64 / dt } else { 0.0 }
                } else {
                    0.0
                };
                MsgRow {
                    id,
                    ext,
                    name: m.map(|m| m.name.clone()),
                    count: idx.len() as u64,
                    data: f.data.to_vec(),
                    t: stats::time_s(&self.ds, f.ts_ns, t0),
                    hz,
                    signals: m.map(|m| decode_message(m, f.data)).unwrap_or_default(),
                }
            })
            .collect()
    }

    /// Signal samples between t0 and t1 (plot seconds), reduced to about
    /// `px` buckets with min and max kept. Returns interleaved [t, v, t, v...].
    pub fn series(&mut self, bus: &str, msg: &str, sig: &str, t0: f64, t1: f64, px: usize) -> Vec<f64> {
        let Some(b) = self.ds.bus_names.iter().position(|n| n == bus) else { return Vec::new() };
        let b = b as u16;
        let Some(m) = self.message_by_name(bus, msg).cloned() else { return Vec::new() };
        let Some(s) = m.signal(sig).cloned() else { return Vec::new() };
        let key = msg_key(m.id, m.extended);
        let cache_key = (b, key, format!("{msg}\u{0}{sig}"));
        let gen = self.ds.generation;
        let base = self.ds.ts.first().copied().unwrap_or(0);
        let ds = &self.ds;
        let e = self.series.entry(cache_key).or_default();
        if e.generation != gen {
            *e = Series { generation: gen, ..Default::default() };
        }
        let idx = ds.frames_of(b, key);
        for &i in &idx[e.upto.min(idx.len())..] {
            let f = ds.frame(i as usize);
            if !signal_active(&m, &s, f.data) {
                continue;
            }
            if let Some(raw) = canlog_dbc_raw(&s, f.data) {
                e.t.push(stats::time_s(ds, f.ts_ns, base));
                e.v.push(raw_to_phys(&s, raw));
            }
        }
        e.upto = idx.len();
        decimate(&e.t, &e.v, t0, t1, px)
    }
}

fn canlog_dbc_raw(s: &canlog_dbc::Signal, data: &[u8]) -> Option<u64> {
    canlog_dbc::raw_bits(s, data)
}

/// Keep first/min/max/last per bucket so spikes survive any zoom level.
pub fn decimate(t: &[f64], v: &[f64], t0: f64, t1: f64, px: usize) -> Vec<f64> {
    let lo = t.partition_point(|&x| x < t0).saturating_sub(1);
    let hi = (t.partition_point(|&x| x <= t1) + 1).min(t.len());
    if lo >= hi {
        return Vec::new();
    }
    let n = hi - lo;
    let mut out = Vec::new();
    if px == 0 || n <= px * 4 {
        out.reserve(n * 2);
        for i in lo..hi {
            out.push(t[i]);
            out.push(v[i]);
        }
        return out;
    }
    let span = (t1 - t0).max(1e-12);
    let mut i = lo;
    while i < hi {
        let bucket = (((t[i] - t0) / span) * px as f64).floor();
        let (start, mut imin, mut imax) = (i, i, i);
        let mut j = i + 1;
        while j < hi && (((t[j] - t0) / span) * px as f64).floor() == bucket {
            if v[j] < v[imin] {
                imin = j;
            }
            if v[j] > v[imax] {
                imax = j;
            }
            j += 1;
        }
        let last = j - 1;
        let mut pts = [start, imin, imax, last];
        pts.sort_unstable();
        let mut prev = usize::MAX;
        for k in pts {
            if k != prev {
                out.push(t[k]);
                out.push(v[k]);
                prev = k;
            }
        }
        i = j;
    }
    out
}

impl Engine {
    /// Response-time analysis for the messages of the DBCs on `bus`.
    pub fn rta(&self, bus: &str, opt: &RtaOptions, overrides: &[RtaOverride]) -> RtaReport {
        let b = self.ds.bus_names.iter().position(|n| n == bus).map(|b| b as u16);
        let mut msgs = Vec::new();
        for info in self.messages(bus) {
            let Some(m) = self.message(bus, info.id, info.ext) else { continue };
            let ov = overrides.iter().find(|o| o.id == m.id && o.ext == m.extended);
            // measured mean frame length, if this message is in the dataset
            let measured_bits = b.and_then(|b| {
                let idx = self.ds.frames_of(b, msg_key(m.id, m.extended));
                if idx.is_empty() {
                    return None;
                }
                let step = (idx.len() / 2000).max(1);
                let (mut sum, mut n) = (0.0, 0);
                for &i in idx.iter().step_by(step) {
                    let f = self.ds.frame(i as usize);
                    sum += crate::bits::frame_bits(&f.shape(), f.data).total() as f64;
                    n += 1;
                }
                Some(sum / n as f64)
            });
            msgs.push(RtaMsg {
                name: m.name.clone(),
                id: m.id,
                ext: m.extended,
                fd: m.fd,
                brs: m.brs,
                len: m.size,
                period_ms: ov.and_then(|o| o.period_ms).or(m.cycle_ms),
                jitter_ms: ov.and_then(|o| o.jitter_ms).unwrap_or(0.0),
                deadline_ms: ov.and_then(|o| o.deadline_ms),
                measured_bits,
            });
        }
        rta::analyse(&msgs, opt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DBC: &str = "BO_ 256 Eng: 8 E\n SG_ Speed : 0|16@1+ (0.25,0) [0|0] \"rpm\" X\nBA_ \"GenMsgCycleTime\" BO_ 256 10;\n";

    #[test]
    fn import_decode_series() {
        let mut e = Engine::new();
        e.load_dbc("eng", DBC.as_bytes());
        e.assign("can0", vec!["eng".into()]);
        e.import_begin();
        let mut log = String::new();
        for i in 0..1000 {
            let raw = (i * 4) as u16;
            log.push_str(&format!("({}.{:06}) can0 100#{:02X}{:02X}\n", 1000 + i / 100, (i % 100) * 10000, raw & 0xFF, raw >> 8));
        }
        e.import_push(log.as_bytes());
        let s = e.import_end();
        assert_eq!(s.frames, 1000);
        let rows = e.latest("can0");
        assert_eq!(rows[0].name.as_deref(), Some("Eng"));
        assert_eq!(rows[0].signals[0].value, 999.0);
        let all = e.series("can0", "Eng", "Speed", 0.0, 1e12, 0);
        assert_eq!(all.len(), 2000);
        let few = e.series("can0", "Eng", "Speed", 0.0, 10.0, 10);
        assert!(few.len() < 200 && few.len() >= 20);
        // min and max survive decimation
        let vals: Vec<f64> = few.iter().skip(1).step_by(2).copied().collect();
        assert_eq!(vals.iter().cloned().fold(f64::MAX, f64::min), 0.0);
        assert_eq!(vals.iter().cloned().fold(f64::MIN, f64::max), 999.0);
        let r = e.rta("can0", &RtaOptions { bitrate: 500_000, dbitrate: 0, sim_seconds: 0.5, sim_runs: 1 }, &[]);
        assert_eq!(r.messages.len(), 1);
        let enc = e.encode("can0", "Eng", &[("Speed".into(), 100.0)]).unwrap();
        assert_eq!(&enc[..2], &[0x90, 0x01]);
    }

    #[test]
    fn live_records() {
        let mut e = Engine::new();
        e.live_begin(&["chassis".into()]);
        let mut buf = Vec::new();
        for i in 0..3u8 {
            buf.extend_from_slice(&(1_000_000_000i64 * i as i64).to_le_bytes());
            buf.extend_from_slice(&0x123u32.to_le_bytes());
            buf.extend_from_slice(&[0, 0, 2, 0, 0xAA, i]);
        }
        let used = e.live_push(&buf[..buf.len() - 3]);
        assert_eq!(used, 36);
        assert_eq!(e.live_push(&buf[used..]), 18);
        assert_eq!(e.ds.len(), 3);
        assert_eq!(e.ds.bus_names, ["chassis"]);
        assert_eq!(e.ds.frame(2).data, &[0xAA, 2]);
    }
}
