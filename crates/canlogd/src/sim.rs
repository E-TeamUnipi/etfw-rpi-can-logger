//! Simulated CAN traffic (`canlogd --sim`) so the whole stack can be run and
//! tested on a laptop without adapters.

use crate::can::RxFrame;
use canlog_core::format::{CANFD_BRS, CAN_EFF_FLAG};

struct Msg {
    ifindex: i32,
    id: u32,
    period_ns: u64,
    len: u8,
    fd: bool,
    next: u64,
    counter: u32,
}

pub struct Sim {
    msgs: Vec<Msg>,
    t0: u64,
}

pub const SIM0: i32 = -100;
pub const SIM1: i32 = -101;

impl Sim {
    pub fn new(now: u64) -> Sim {
        let mut msgs = Vec::new();
        let mut add = |ifindex, id, hz: u64, len, fd| {
            msgs.push(Msg { ifindex, id, period_ns: 1_000_000_000 / hz, len, fd, next: now + (id as u64 % 97) * 100_000, counter: 0 });
        };
        // powertrain-ish bus, 500 kbit/s
        for (id, hz) in [(0x0A0, 100), (0x0C1, 100), (0x1A0, 50), (0x201, 20), (0x2F0, 10), (0x350, 10), (0x3E9, 5), (0x420, 1), (0x5F0, 1), (0x7DF, 1)] {
            add(SIM0, id, hz, 8, false);
        }
        // body bus with J1939-style extended ids and a few CAN FD frames
        for (id, hz) in [(0x0CF00400, 50), (0x18FEF100, 10), (0x18FEEE00, 1), (0x18FEE900, 1)] {
            add(SIM1, id | CAN_EFF_FLAG, hz, 8, false);
        }
        add(SIM1, 0x123, 20, 32, true);
        add(SIM1, 0x456, 5, 64, true);
        Sim { msgs, t0: now }
    }

    /// Frames due up to `now` (boot clock ns). Timestamps are given as
    /// realtime so they go through the same conversion as real frames.
    pub fn poll(&mut self, now: u64, rt_offset: i64, out: &mut Vec<RxFrame>) {
        let t_s = (now - self.t0) as f64 / 1e9;
        for m in &mut self.msgs {
            while m.next <= now && out.len() < 4096 {
                let mut data = [0u8; 64];
                let c = m.counter;
                match m.id {
                    0x0A0 => {
                        // "rpm" and "speed"
                        let rpm = (2000.0 + 1500.0 * (t_s * 0.3).sin()) as u16;
                        let spd = (60.0 + 40.0 * (t_s * 0.05).sin()) as u16 * 100;
                        data[0..2].copy_from_slice(&rpm.to_be_bytes());
                        data[2..4].copy_from_slice(&spd.to_be_bytes());
                        data[7] = (c & 0x0F) as u8;
                    }
                    _ => {
                        for (i, d) in data.iter_mut().enumerate().take(m.len as usize) {
                            *d = (m.id as u8).wrapping_mul(7).wrapping_add(i as u8 * 13);
                        }
                        data[0] = c as u8;
                    }
                }
                out.push(RxFrame {
                    ifindex: m.ifindex,
                    ts_rt_ns: Some(m.next as i64 + rt_offset),
                    id: m.id,
                    fd: m.fd,
                    flags: if m.fd { CANFD_BRS } else { 0 },
                    len: m.len,
                    data,
                    local: false,
                });
                m.counter = m.counter.wrapping_add(1);
                m.next += m.period_ns;
            }
        }
        out.sort_by_key(|f| f.ts_rt_ns);
    }
}
