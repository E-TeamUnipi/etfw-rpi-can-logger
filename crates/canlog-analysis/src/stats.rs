//! Measured statistics: bus load (average and peak over sliding windows)
//! and per-message timing.

use crate::bits::frame_bits;
use crate::dataset::*;
use serde::Serialize;

/// Window lengths for peak load (ns).
pub const WINDOWS_NS: [i64; 3] = [10_000_000, 100_000_000, 1_000_000_000];

#[derive(Debug, Clone, Serialize, Default)]
pub struct BusLoad {
    pub bus: String,
    pub bitrate: u32,
    pub dbitrate: u32,
    pub frames: u64,
    pub error_frames: u64,
    pub tx_frames: u64,
    pub duration_s: f64,
    /// Average over the whole recording (%).
    pub avg_pct: f64,
    /// Peak over 10 ms, 100 ms and 1 s windows (%).
    pub peak_pct: [f64; 3],
    /// Start time of the 10 ms peak window (s, same clock as the series).
    pub peak_at_s: f64,
    /// Load per 1 s window, for plotting: (window start s, %).
    pub timeline: Vec<(f64, f64)>,
    pub frames_per_s: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MsgStats {
    pub bus: String,
    pub id: u32,
    pub ext: bool,
    pub count: u64,
    pub len: u8,
    pub fd: bool,
    /// Mean, min and max interval between frames (ms).
    pub period_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    /// Standard deviation of the interval (ms).
    pub jitter_ms: f64,
    /// Share of this message in the bus load (% of bus time).
    pub load_pct: f64,
    /// Mean frame length on the wire (bits, incl. stuffing and IFS).
    pub mean_bits: f64,
    pub last_data: Vec<u8>,
}

struct Window {
    len: i64,
    cur: i64,
    acc: f64,
    peak: f64,
    peak_at: i64,
}

impl Window {
    fn add(&mut self, ts: i64, busy: f64, timeline: Option<&mut Vec<(i64, f64)>>) {
        let b = ts.div_euclid(self.len);
        if b != self.cur {
            self.close(timeline);
            self.cur = b;
            self.acc = 0.0;
        }
        self.acc += busy;
    }
    fn close(&mut self, timeline: Option<&mut Vec<(i64, f64)>>) {
        if self.cur == i64::MIN {
            return;
        }
        if self.acc > self.peak {
            self.peak = self.acc;
            self.peak_at = self.cur * self.len;
        }
        if let Some(t) = timeline {
            t.push((self.cur * self.len, self.acc));
        }
    }
}

/// Bitrates per bus index (nominal, data). 0 = unknown: load is not computed.
pub fn bus_load(ds: &Dataset, bitrates: &[(u32, u32)]) -> Vec<BusLoad> {
    let nb = ds.bus_names.len();
    let mut out: Vec<BusLoad> = (0..nb)
        .map(|b| {
            let (br, dbr) = bitrates.get(b).copied().unwrap_or((0, 0));
            BusLoad { bus: ds.bus_names[b].clone(), bitrate: br, dbitrate: dbr, ..Default::default() }
        })
        .collect();
    let mut wins: Vec<Vec<Window>> = (0..nb)
        .map(|_| WINDOWS_NS.iter().map(|&l| Window { len: l, cur: i64::MIN, acc: 0.0, peak: 0.0, peak_at: 0 }).collect())
        .collect();
    let mut timelines: Vec<Vec<(i64, f64)>> = vec![Vec::new(); nb];
    let mut busy = vec![0f64; nb];
    let mut first = vec![i64::MAX; nb];
    let mut last = vec![i64::MIN; nb];
    for i in 0..ds.len() {
        let f = ds.frame(i);
        let b = f.bus as usize;
        let o = &mut out[b];
        first[b] = first[b].min(f.ts_ns);
        last[b] = last[b].max(f.ts_ns);
        if f.flags & F_ERR != 0 {
            o.error_frames += 1;
            continue;
        }
        o.frames += 1;
        if f.flags & F_TX != 0 {
            o.tx_frames += 1;
        }
        if o.bitrate == 0 {
            continue;
        }
        let ns = frame_bits(&f.shape(), f.data).duration_ns(o.bitrate, o.dbitrate);
        busy[b] += ns;
        let (w, rest) = wins[b].split_at_mut(2);
        w[0].add(f.ts_ns, ns, None);
        w[1].add(f.ts_ns, ns, None);
        rest[0].add(f.ts_ns, ns, Some(&mut timelines[b]));
    }
    let t0 = ds.ts.first().copied().unwrap_or(0);
    for b in 0..nb {
        let o = &mut out[b];
        if first[b] == i64::MAX {
            continue;
        }
        // a recording of one frame still spans that frame
        let dur = ((last[b] - first[b]) as f64).max(1e6);
        o.duration_s = dur / 1e9;
        o.frames_per_s = o.frames as f64 / o.duration_s;
        if o.bitrate == 0 {
            continue;
        }
        o.avg_pct = (busy[b] / dur * 100.0).min(100.0);
        for (k, w) in wins[b].iter_mut().enumerate() {
            let tl = if k == 2 { Some(&mut timelines[b]) } else { None };
            w.close(tl);
            o.peak_pct[k] = (w.peak / w.len as f64 * 100.0).min(100.0);
        }
        o.peak_at_s = time_s(ds, wins[b][0].peak_at, t0);
        o.timeline = timelines[b].iter().map(|&(t, v)| (time_s(ds, t, t0), (v / 1e9 * 100.0).min(100.0))).collect();
    }
    out
}

/// Seconds on the axis used for plots: Unix time when the dataset has wall
/// clock time, otherwise relative to the first frame.
pub fn time_s(ds: &Dataset, ts_ns: i64, t0: i64) -> f64 {
    if ds.wall_clock {
        ts_ns as f64 / 1e9
    } else {
        (ts_ns - t0) as f64 / 1e9
    }
}

/// Per-message timing on one bus.
pub fn msg_stats(ds: &Dataset, bus: u16, bitrate: u32, dbitrate: u32) -> Vec<MsgStats> {
    let dur_ns = ds.time_range().map(|(a, b)| (b - a).max(1)).unwrap_or(1) as f64;
    let mut keys: Vec<u32> = ds.keys().filter(|k| k.0 == bus).map(|k| k.1).collect();
    keys.sort_unstable_by_key(|k| (k & 0x8000_0000 != 0, k & 0x1FFF_FFFF));
    keys.into_iter()
        .map(|key| {
            let idx = ds.frames_of(bus, key);
            let mut s = MsgStats {
                bus: ds.bus_names[bus as usize].clone(),
                id: key & 0x1FFF_FFFF,
                ext: key & 0x8000_0000 != 0,
                count: idx.len() as u64,
                min_ms: f64::INFINITY,
                ..Default::default()
            };
            let (mut sum, mut sum2, mut n) = (0.0, 0.0, 0u64);
            let mut bits = 0f64;
            let mut busy = 0f64;
            let mut prev: Option<i64> = None;
            for &i in idx {
                let f = ds.frame(i as usize);
                let b = frame_bits(&f.shape(), f.data);
                bits += b.total() as f64;
                busy += b.duration_ns(bitrate, dbitrate);
                if let Some(p) = prev {
                    let d = (f.ts_ns - p) as f64 / 1e6;
                    sum += d;
                    sum2 += d * d;
                    n += 1;
                    s.min_ms = s.min_ms.min(d);
                    s.max_ms = s.max_ms.max(d);
                }
                prev = Some(f.ts_ns);
            }
            if let Some(&i) = idx.last() {
                let f = ds.frame(i as usize);
                s.len = f.data.len() as u8;
                s.fd = f.flags & F_FD != 0;
                s.last_data = f.data.to_vec();
            }
            if n > 0 {
                s.period_ms = sum / n as f64;
                s.jitter_ms = (sum2 / n as f64 - s.period_ms * s.period_ms).max(0.0).sqrt();
            } else {
                s.min_ms = 0.0;
            }
            s.mean_bits = bits / idx.len().max(1) as f64;
            if bitrate > 0 {
                s.load_pct = busy / dur_ns * 100.0;
            }
            s
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_of_periodic_traffic() {
        let mut ds = Dataset::default();
        let b = ds.bus_index("can0");
        // one 8-byte frame every 1 ms for 2 s at 500 kbit/s: ~23-27 %
        for i in 0..2000i64 {
            ds.push(i * 1_000_000, b, 0x100, 0, &(i as u64).to_le_bytes());
        }
        // a burst: 20 frames back to back at t = 1.5 s
        for k in 0..20i64 {
            ds.push(1_500_000_000 + k * 1000, b, 0x50, 0, &[0xFF; 8]);
        }
        let mut sorted: Vec<usize> = (0..ds.len()).collect();
        sorted.sort_by_key(|&i| ds.ts[i]);
        let mut s2 = Dataset::default();
        let b2 = s2.bus_index("can0");
        for i in sorted {
            let f = ds.frame(i);
            s2.push(f.ts_ns, b2, f.id, f.flags, f.data);
        }
        let l = &bus_load(&s2, &[(500_000, 0)])[0];
        assert_eq!(l.frames, 2020);
        assert!(l.avg_pct > 20.0 && l.avg_pct < 30.0, "{}", l.avg_pct);
        assert!(l.peak_pct[0] > l.peak_pct[1] && l.peak_pct[1] >= l.peak_pct[2] * 0.99, "{:?}", l.peak_pct);
        assert!((l.peak_at_s - 1.5).abs() < 0.011);
        assert_eq!(l.timeline.len(), 2);
        let m = msg_stats(&s2, 0, 500_000, 0);
        assert_eq!(m.len(), 2);
        let m100 = m.iter().find(|m| m.id == 0x100).unwrap();
        assert!((m100.period_ms - 1.0).abs() < 1e-9 && m100.jitter_ms < 1e-6);
    }
}
