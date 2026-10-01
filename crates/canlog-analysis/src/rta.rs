//! Response time of CAN messages from a DBC: the time from when a message
//! is queued (released) to when its transmission completes.
//!
//! - Worst case: Davis, Burns, Bril, Lukkien, "Controller Area Network (CAN)
//!   schedulability analysis: Refuted, revisited and revised", Real-Time
//!   Systems 35(3), 2007. Fixed priorities, non-preemptive, worst-case bit
//!   stuffing, no errors, every controller sends in priority order.
//! - Average case: a simulation of the bus with random release phases,
//!   per-frame stuffing from random (or measured) payloads.

use crate::bits::{random_bits, worst_bits, Rng, Shape};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct RtaMsg {
    pub name: String,
    pub id: u32,
    pub ext: bool,
    pub fd: bool,
    pub brs: bool,
    pub len: u8,
    /// Period or minimum inter-arrival time (ms). None = unknown (event
    /// message without a minimum gap): left out of interference.
    pub period_ms: Option<f64>,
    /// Queuing jitter (ms).
    #[serde(default)]
    pub jitter_ms: f64,
    /// Deadline (ms), default = period.
    #[serde(default)]
    pub deadline_ms: Option<f64>,
    /// Mean frame length measured in a log (bits), if available.
    #[serde(default)]
    pub measured_bits: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RtaResult {
    pub name: String,
    pub id: u32,
    pub ext: bool,
    pub period_ms: Option<f64>,
    pub deadline_ms: Option<f64>,
    /// Transmission time, worst-case stuffing (ms).
    pub c_worst_ms: f64,
    /// Mean transmission time (ms).
    pub c_avg_ms: f64,
    /// Longest blocking by a lower-priority frame (ms).
    pub blocking_ms: f64,
    /// Worst-case response time (ms); None if unbounded (bus overloaded).
    pub r_worst_ms: Option<f64>,
    /// Mean response time in the simulation (ms).
    pub r_avg_ms: Option<f64>,
    /// Longest response time seen in the simulation (ms).
    pub r_sim_max_ms: Option<f64>,
    pub schedulable: Option<bool>,
    pub sim_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RtaReport {
    pub bitrate: u32,
    pub dbitrate: u32,
    /// Utilisation with worst-case and mean frame lengths (%).
    pub util_worst_pct: f64,
    pub util_avg_pct: f64,
    pub messages: Vec<RtaResult>,
    pub unknown_period: usize,
    pub sim_seconds: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RtaOptions {
    pub bitrate: u32,
    #[serde(default)]
    pub dbitrate: u32,
    /// Simulated time per run (s).
    #[serde(default = "default_sim_s")]
    pub sim_seconds: f64,
    #[serde(default = "default_runs")]
    pub sim_runs: u32,
}

fn default_sim_s() -> f64 {
    10.0
}
fn default_runs() -> u32 {
    4
}

/// Arbitration order: smaller key wins. A standard frame beats an extended
/// one with the same 11-bit base id (RTR/SRR and IDE bits).
fn prio_key(id: u32, ext: bool) -> u64 {
    if ext {
        ((id >> 18 & 0x7FF) as u64) << 20 | 1 << 19 | (id & 0x3FFFF) as u64
    } else {
        ((id & 0x7FF) as u64) << 20
    }
}

struct M {
    c: f64,
    c_avg: f64,
    /// mean and spread of per-frame times for the simulation
    c_samples: Vec<f64>,
    t: Option<f64>,
    j: f64,
}

pub fn analyse(msgs: &[RtaMsg], opt: &RtaOptions) -> RtaReport {
    let mut order: Vec<usize> = (0..msgs.len()).collect();
    order.sort_by_key(|&i| prio_key(msgs[i].id, msgs[i].ext));
    let ns = |bits: crate::bits::Bits| bits.duration_ns(opt.bitrate, opt.dbitrate);
    let tau = if opt.bitrate > 0 { 1e9 / opt.bitrate as f64 } else { 0.0 };

    // in priority order
    let ms: Vec<M> = order
        .iter()
        .map(|&i| {
            let m = &msgs[i];
            let shape = Shape { id: m.id, ext: m.ext, rtr: false, fd: m.fd, brs: m.brs };
            let len = m.len as usize;
            let c = ns(worst_bits(&shape, len));
            let (mean_bits, _) = random_bits(&shape, len, 64);
            let mut c_samples = Vec::with_capacity(16);
            let mut rng = Rng(0xC0FFEE ^ m.id as u64);
            let mut data = vec![0u8; len];
            for _ in 0..16 {
                for x in data.iter_mut() {
                    *x = rng.next() as u8;
                }
                c_samples.push(ns(crate::bits::frame_bits(&shape, &data)));
            }
            // measured payloads stuff differently from random ones: scale
            let c_avg_random = c_samples.iter().sum::<f64>() / c_samples.len() as f64;
            let c_avg = match m.measured_bits {
                Some(b) if b > 0.0 && mean_bits > 0.0 => {
                    let k = b / mean_bits;
                    for s in c_samples.iter_mut() {
                        *s *= k;
                    }
                    c_avg_random * k
                }
                _ => c_avg_random,
            };
            let t = m.period_ms.filter(|p| *p > 0.0).map(|p| p * 1e6);
            M { c, c_avg, c_samples, t, j: m.jitter_ms * 1e6 }
        })
        .collect();

    let n = ms.len();
    let util_worst: f64 = ms.iter().filter_map(|m| m.t.map(|t| m.c / t)).sum();
    let util_avg: f64 = ms.iter().filter_map(|m| m.t.map(|t| m.c_avg / t)).sum();

    let sim = simulate(&ms, opt);

    let mut results = Vec::with_capacity(n);
    for p in 0..n {
        let m = &ms[p];
        let src = &msgs[order[p]];
        // blocking: longest lower-priority frame
        let b = ms[p + 1..].iter().map(|x| x.c).fold(0.0, f64::max);
        let r = wcrt(&ms, p, b, tau);
        let deadline = src.deadline_ms.or(src.period_ms).filter(|d| *d > 0.0);
        let (sum, max, count) = sim[p];
        results.push(RtaResult {
            name: src.name.clone(),
            id: src.id,
            ext: src.ext,
            period_ms: src.period_ms,
            deadline_ms: deadline,
            c_worst_ms: m.c / 1e6,
            c_avg_ms: m.c_avg / 1e6,
            blocking_ms: b / 1e6,
            r_worst_ms: r.map(|r| r / 1e6),
            r_avg_ms: (count > 0).then(|| sum / count as f64 / 1e6),
            r_sim_max_ms: (count > 0).then(|| max / 1e6),
            schedulable: deadline.map(|d| r.is_some_and(|r| r <= d * 1e6)),
            sim_count: count,
        });
    }
    RtaReport {
        bitrate: opt.bitrate,
        dbitrate: opt.dbitrate,
        util_worst_pct: util_worst * 100.0,
        util_avg_pct: util_avg * 100.0,
        unknown_period: ms.iter().filter(|m| m.t.is_none()).count(),
        messages: results,
        sim_seconds: opt.sim_seconds * opt.sim_runs as f64,
    }
}

/// Worst-case response time of message `p` (index in priority order), ns.
fn wcrt(ms: &[M], p: usize, b: f64, tau: f64) -> Option<f64> {
    let m = &ms[p];
    let hp = &ms[..p];
    // utilisation of higher-or-equal priority messages must be < 1
    let u: f64 = ms[..=p].iter().filter_map(|x| x.t.map(|t| x.c / t)).sum();
    if u >= 1.0 {
        return None;
    }
    let limit = 60e9; // 60 s: anything longer is unbounded in practice
    // level-m busy period
    let mut t = m.c;
    loop {
        let next = b + ms[..=p].iter().map(|x| x.t.map(|tk| ((t + x.j) / tk).ceil() * x.c).unwrap_or(if std::ptr::eq(x, m) { x.c } else { 0.0 })).sum::<f64>();
        if next > limit {
            return None;
        }
        if (next - t).abs() < 1e-6 {
            break;
        }
        t = next;
    }
    let q_count = match m.t {
        Some(tm) => (((t + m.j) / tm).ceil() as u64).clamp(1, 100_000),
        None => 1,
    };
    let mut r_max: f64 = 0.0;
    for q in 0..q_count {
        let qf = q as f64;
        let mut w = b + qf * m.c;
        loop {
            let next = b + qf * m.c + hp.iter().filter_map(|x| x.t.map(|tk| ((w + x.j + tau) / tk).ceil() * x.c)).sum::<f64>();
            if next > limit {
                return None;
            }
            if (next - w).abs() < 1e-6 {
                break;
            }
            w = next;
        }
        let r = m.j + w - qf * m.t.unwrap_or(0.0) + m.c;
        r_max = r_max.max(r);
    }
    Some(r_max)
}

/// Event-driven bus simulation. Returns per message (sum, max, count) of
/// response times in ns.
fn simulate(ms: &[M], opt: &RtaOptions) -> Vec<(f64, f64, u64)> {
    let n = ms.len();
    let mut out = vec![(0.0, 0.0, 0u64); n];
    let horizon = opt.sim_seconds.max(0.1) * 1e9;
    for run in 0..opt.sim_runs.max(1) {
        let mut rng = Rng(0x5EED_0000 + run as u64 * 7919);
        // next release per message, and queued release times (FIFO)
        let mut next: Vec<f64> = ms.iter().map(|m| m.t.map(|t| rng.unit() * t).unwrap_or(f64::INFINITY)).collect();
        let mut queue: Vec<std::collections::VecDeque<f64>> = vec![Default::default(); n];
        let mut now = 0.0;
        // warm-up: skip statistics for the first period of each message
        let warm: f64 = ms.iter().filter_map(|m| m.t).fold(0.0, f64::max).min(horizon / 4.0);
        while now < horizon {
            // release everything due
            for k in 0..n {
                while next[k] <= now {
                    let t = ms[k].t.unwrap();
                    let jit = if ms[k].j > 0.0 { rng.unit() * ms[k].j } else { 0.0 };
                    queue[k].push_back(next[k] + jit);
                    next[k] += t;
                }
            }
            // highest priority message with a release that has happened
            let pick = (0..n).find(|&k| queue[k].front().is_some_and(|&r| r <= now));
            match pick {
                Some(k) => {
                    let rel = queue[k].pop_front().unwrap();
                    let c = ms[k].c_samples[(rng.next() % ms[k].c_samples.len() as u64) as usize];
                    let done = now + c;
                    if rel >= warm {
                        let r = done - rel;
                        out[k].0 += r;
                        out[k].1 = f64::max(out[k].1, r);
                        out[k].2 += 1;
                    }
                    now = done;
                }
                None => {
                    // idle until the next release (including jittered ones)
                    let q = queue.iter().filter_map(|q| q.front().copied()).fold(f64::INFINITY, f64::min);
                    let r = next.iter().copied().fold(f64::INFINITY, f64::min);
                    let t = q.min(r);
                    if !t.is_finite() {
                        break;
                    }
                    now = now.max(t);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: u32, period: f64) -> RtaMsg {
        RtaMsg { name: format!("M{id:X}"), id, ext: false, fd: false, brs: false, len: 8, period_ms: Some(period), jitter_ms: 0.0, deadline_ms: None, measured_bits: None }
    }

    #[test]
    fn single_message() {
        let r = analyse(&[msg(0x100, 10.0)], &RtaOptions { bitrate: 500_000, dbitrate: 0, sim_seconds: 1.0, sim_runs: 1 });
        let m = &r.messages[0];
        // 135 bits at 500 kbit/s = 0.27 ms, no blocking, no interference
        assert!((m.c_worst_ms - 0.27).abs() < 1e-9);
        assert!((m.r_worst_ms.unwrap() - 0.27).abs() < 1e-9);
        assert!(m.r_avg_ms.unwrap() <= 0.27 && m.r_avg_ms.unwrap() > 0.2);
        assert_eq!(m.schedulable, Some(true));
    }

    #[test]
    fn simulation_never_exceeds_worst_case() {
        let mut v = Vec::new();
        for (i, p) in [5.0, 10.0, 10.0, 20.0, 20.0, 50.0, 100.0, 100.0, 1000.0, 7.0, 3.0, 13.0].iter().enumerate() {
            v.push(msg(0x80 + i as u32 * 16, *p));
        }
        v.push(RtaMsg { period_ms: None, ..msg(0x700, 0.0) });
        let r = analyse(&v, &RtaOptions { bitrate: 250_000, dbitrate: 0, sim_seconds: 5.0, sim_runs: 3 });
        assert!(r.util_worst_pct > 30.0 && r.util_worst_pct < 100.0, "{}", r.util_worst_pct);
        assert_eq!(r.unknown_period, 1);
        for m in &r.messages {
            if let (Some(w), Some(s)) = (m.r_worst_ms, m.r_sim_max_ms) {
                assert!(s <= w + 1e-6, "{}: sim {s} > wcrt {w}", m.name);
                assert!(m.r_avg_ms.unwrap() <= s);
            }
        }
        // priority order: lower ids first, response times grow down the list
        assert!(r.messages[0].r_worst_ms.unwrap() < r.messages[11].r_worst_ms.unwrap());
        // lowest priority message has no blocking
        assert_eq!(r.messages.last().unwrap().blocking_ms, 0.0);
    }

    #[test]
    fn overload_is_unbounded() {
        let v: Vec<_> = (0..10).map(|i| msg(0x100 + i, 1.0)).collect();
        let r = analyse(&v, &RtaOptions { bitrate: 125_000, dbitrate: 0, sim_seconds: 0.5, sim_runs: 1 });
        assert!(r.util_worst_pct > 100.0);
        assert!(r.messages.last().unwrap().r_worst_ms.is_none());
        assert_eq!(r.messages.last().unwrap().schedulable, Some(false));
    }
}

#[cfg(test)]
mod prio_tests {
    use super::*;
    #[test]
    fn extended_vs_standard_priority() {
        let mk = |id: u32, ext: bool| RtaMsg { name: format!("{id:X}"), id, ext, fd: false, brs: false, len: 8, period_ms: Some(10.0), jitter_ms: 0.0, deadline_ms: None, measured_bits: None };
        // 0x0CF00400 has base id 0x33C; 0x00CC0001 has base id 0x033
        let r = analyse(&[mk(0x123, false), mk(0x0CF00400, true), mk(0x033, false), mk(0x00CC0001, true)], &RtaOptions { bitrate: 500_000, dbitrate: 0, sim_seconds: 0.2, sim_runs: 1 });
        let names: Vec<_> = r.messages.iter().map(|m| m.name.as_str()).collect();
        // a standard frame beats an extended one with the same base id
        assert_eq!(names, ["33", "CC0001", "123", "CF00400"]);
    }
}
