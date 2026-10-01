//! Frames in columns (compact enough for millions of frames in a browser),
//! with a per-message index built on demand.

use crate::bits::Shape;
use std::collections::HashMap;

pub const F_EXT: u8 = 1;
pub const F_RTR: u8 = 2;
pub const F_ERR: u8 = 4;
pub const F_FD: u8 = 8;
pub const F_BRS: u8 = 16;
pub const F_ESI: u8 = 32;
/// Sent by the logger (or marked Tx in the log).
pub const F_TX: u8 = 64;

/// One frame, borrowed from a dataset.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    pub ts_ns: i64,
    pub bus: u16,
    pub id: u32,
    pub flags: u8,
    pub data: &'a [u8],
}

impl Frame<'_> {
    pub fn ext(&self) -> bool {
        self.flags & F_EXT != 0
    }
    pub fn shape(&self) -> Shape {
        Shape {
            id: self.id,
            ext: self.flags & F_EXT != 0,
            rtr: self.flags & F_RTR != 0,
            fd: self.flags & F_FD != 0,
            brs: self.flags & F_BRS != 0,
        }
    }
}

/// Message key within a bus: id plus the extended flag.
pub fn msg_key(id: u32, ext: bool) -> u32 {
    if ext {
        id | 0x8000_0000
    } else {
        id
    }
}

#[derive(Default)]
pub struct Dataset {
    pub ts: Vec<i64>,
    pub bus: Vec<u16>,
    pub id: Vec<u32>,
    pub flags: Vec<u8>,
    pub len: Vec<u8>,
    off: Vec<u32>,
    data: Vec<u8>,
    pub bus_names: Vec<String>,
    /// Timestamps are Unix time (ns) rather than an arbitrary clock.
    pub wall_clock: bool,
    pub markers: Vec<(i64, String)>,
    /// (bus, msg_key) -> frame indices, kept current by `push`.
    index: HashMap<(u16, u32), Vec<u32>>,
    /// Frames dropped from the front (live capping); lets caches detect it.
    pub generation: u32,
}

impl Dataset {
    /// An empty dataset that invalidates caches made for this one.
    pub fn next(&self, wall_clock: bool) -> Dataset {
        Dataset { generation: self.generation + 1, wall_clock, ..Default::default() }
    }

    pub fn len(&self) -> usize {
        self.ts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }

    pub fn bus_index(&mut self, name: &str) -> u16 {
        if let Some(i) = self.bus_names.iter().position(|n| n == name) {
            return i as u16;
        }
        self.bus_names.push(name.to_string());
        (self.bus_names.len() - 1) as u16
    }

    pub fn push(&mut self, ts_ns: i64, bus: u16, id: u32, flags: u8, data: &[u8]) {
        let i = self.ts.len() as u32;
        self.ts.push(ts_ns);
        self.bus.push(bus);
        self.id.push(id);
        self.flags.push(flags);
        self.len.push(data.len() as u8);
        self.off.push(self.data.len() as u32);
        self.data.extend_from_slice(data);
        if flags & F_ERR == 0 {
            self.index.entry((bus, msg_key(id, flags & F_EXT != 0))).or_default().push(i);
        }
    }

    pub fn frame(&self, i: usize) -> Frame<'_> {
        let o = self.off[i] as usize;
        Frame { ts_ns: self.ts[i], bus: self.bus[i], id: self.id[i], flags: self.flags[i], data: &self.data[o..o + self.len[i] as usize] }
    }

    /// Frame indices of one message on one bus, in arrival order.
    pub fn frames_of(&self, bus: u16, key: u32) -> &[u32] {
        self.index.get(&(bus, key)).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// All (bus, msg_key) pairs seen.
    pub fn keys(&self) -> impl Iterator<Item = &(u16, u32)> {
        self.index.keys()
    }

    pub fn time_range(&self) -> Option<(i64, i64)> {
        Some((*self.ts.first()?, *self.ts.last()?))
    }

    pub fn clear(&mut self) {
        let names = std::mem::take(&mut self.bus_names);
        let generation = self.generation + 1;
        *self = Dataset { bus_names: names, generation, ..Default::default() };
    }

    /// Keep only the newest `keep` frames (live views with a memory cap).
    pub fn drop_front(&mut self, keep: usize) {
        if self.len() <= keep {
            return;
        }
        let cut = self.len() - keep;
        let data_cut = self.off[cut] as usize;
        self.ts.drain(..cut);
        self.bus.drain(..cut);
        self.id.drain(..cut);
        self.flags.drain(..cut);
        self.len.drain(..cut);
        self.off.drain(..cut);
        for o in &mut self.off {
            *o -= data_cut as u32;
        }
        self.data.drain(..data_cut);
        self.index.clear();
        for i in 0..self.ts.len() {
            if self.flags[i] & F_ERR == 0 {
                self.index.entry((self.bus[i], msg_key(self.id[i], self.flags[i] & F_EXT != 0))).or_default().push(i as u32);
            }
        }
        let t0 = self.ts.first().copied().unwrap_or(i64::MIN);
        self.markers.retain(|m| m.0 >= t0);
        self.generation += 1;
    }

    /// Approximate memory use in bytes.
    pub fn memory(&self) -> usize {
        self.ts.len() * (8 + 2 + 4 + 1 + 1 + 4 + 4) + self.data.len()
    }
}
