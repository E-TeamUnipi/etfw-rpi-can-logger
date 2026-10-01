//! Signal extraction and insertion.

use crate::{Message, Mux, Signal, ValueType};
use serde::Serialize;

fn mask(size: u16) -> u64 {
    if size >= 64 {
        u64::MAX
    } else {
        (1u64 << size) - 1
    }
}

/// Raw bits of a signal, or None if it does not fit in `data`.
pub fn raw_bits(s: &Signal, data: &[u8]) -> Option<u64> {
    let (start, size) = (s.start_bit as usize, s.size as usize);
    if data.len() <= 8 {
        let mut d = [0u8; 8];
        d[..data.len()].copy_from_slice(data);
        let have = data.len() * 8;
        if s.little_endian {
            if start + size > have {
                return None;
            }
            Some((u64::from_le_bytes(d) >> start) & mask(s.size))
        } else {
            let msb = (start / 8) * 8 + (7 - start % 8);
            let lsb = msb + size - 1;
            if lsb >= have {
                return None;
            }
            Some((u64::from_be_bytes(d) >> (63 - lsb)) & mask(s.size))
        }
    } else {
        // CAN FD payloads: bit by bit
        let mut raw = 0u64;
        if s.little_endian {
            for i in 0..size {
                let p = start + i;
                let b = *data.get(p / 8)? >> (p % 8) & 1;
                raw |= (b as u64) << i;
            }
        } else {
            let mut p = start;
            for _ in 0..size {
                let b = *data.get(p / 8)? >> (p % 8) & 1;
                raw = raw << 1 | b as u64;
                p = if p % 8 == 0 { p + 15 } else { p - 1 };
            }
        }
        Some(raw)
    }
}

fn put_bits(s: &Signal, data: &mut [u8], raw: u64) {
    let raw = raw & mask(s.size);
    let (start, size) = (s.start_bit as usize, s.size as usize);
    let mut set = |p: usize, b: u64| {
        if let Some(byte) = data.get_mut(p / 8) {
            *byte = (*byte & !(1 << (p % 8))) | ((b as u8) << (p % 8));
        }
    };
    if s.little_endian {
        for i in 0..size {
            set(start + i, raw >> i & 1);
        }
    } else {
        let mut p = start;
        for i in (0..size).rev() {
            set(p, raw >> i & 1);
            p = if p % 8 == 0 { p + 15 } else { p.wrapping_sub(1) };
        }
    }
}

/// Raw integer value (sign-extended if the signal is signed).
fn raw_int(s: &Signal, raw: u64) -> i128 {
    if s.signed && s.size < 64 && raw >> (s.size - 1) & 1 == 1 {
        (raw | !mask(s.size)) as i64 as i128
    } else if s.signed {
        raw as i64 as i128
    } else {
        raw as i128
    }
}

/// Physical value from raw bits.
pub fn raw_to_phys(s: &Signal, raw: u64) -> f64 {
    let v = match s.value_type {
        ValueType::Float32 => f32::from_bits(raw as u32) as f64,
        ValueType::Float64 => f64::from_bits(raw),
        ValueType::Int => raw_int(s, raw) as f64,
    };
    v * s.factor + s.offset
}

fn phys_to_raw(s: &Signal, phys: f64) -> u64 {
    let v = if s.factor != 0.0 { (phys - s.offset) / s.factor } else { 0.0 };
    match s.value_type {
        ValueType::Float32 => (v as f32).to_bits() as u64,
        ValueType::Float64 => v.to_bits(),
        ValueType::Int => {
            let v = v.round();
            if s.signed {
                let lo = -(1i128 << (s.size - 1));
                let hi = (1i128 << (s.size - 1)) - 1;
                (v.clamp(lo as f64, hi as f64) as i128) as u64
            } else {
                let hi = mask(s.size);
                v.clamp(0.0, hi as f64) as u64
            }
        }
    }
}

/// Physical value of one signal (ignores multiplexing; see `signal_active`).
pub fn decode_signal(s: &Signal, data: &[u8]) -> Option<f64> {
    raw_bits(s, data).map(|r| raw_to_phys(s, r))
}

/// Whether a (possibly multiplexed) signal is present in this payload.
pub fn signal_active(m: &Message, s: &Signal, data: &[u8]) -> bool {
    active_depth(m, s, data, 0)
}

fn active_depth(m: &Message, s: &Signal, data: &[u8], depth: u8) -> bool {
    if depth > 8 {
        return false;
    }
    if !s.mux_ext.is_empty() {
        return s.mux_ext.iter().all(|r| {
            let Some(sw) = m.signal(&r.switch) else { return false };
            let Some(v) = raw_bits(sw, data) else { return false };
            active_depth(m, sw, data, depth + 1) && r.ranges.iter().any(|&(lo, hi)| v >= lo && v <= hi)
        });
    }
    match s.mux {
        Mux::None | Mux::Multiplexor => true,
        Mux::Multiplexed { value, .. } => match m.multiplexor() {
            Some(sw) => raw_bits(sw, data) == Some(value),
            None => false,
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DecodedSignal {
    pub name: String,
    pub value: f64,
    pub raw: i64,
    pub unit: String,
    /// Value description from `VAL_`, if any.
    pub text: Option<String>,
}

/// All signals present in the payload.
pub fn decode_message(m: &Message, data: &[u8]) -> Vec<DecodedSignal> {
    m.signals
        .iter()
        .filter(|s| signal_active(m, s, data))
        .filter_map(|s| {
            let raw = raw_bits(s, data)?;
            let ri = raw_int(s, raw) as i64;
            Some(DecodedSignal {
                name: s.name.clone(),
                value: raw_to_phys(s, raw),
                raw: ri,
                unit: s.unit.clone(),
                text: s.choice(ri).map(|t| t.to_string()),
            })
        })
        .collect()
}

/// Build a payload of `m.size` bytes. Signals not given use their
/// `GenSigStartValue`, or raw 0. Multiplexors are set first, then only the
/// signals active for the chosen multiplexor values are written.
pub fn encode_message(m: &Message, values: &[(String, f64)]) -> Vec<u8> {
    let mut data = vec![0u8; m.size as usize];
    let get = |s: &Signal| values.iter().find(|(n, _)| *n == s.name).map(|(_, v)| *v);
    let is_switch = |s: &Signal| {
        matches!(s.mux, Mux::Multiplexor | Mux::Multiplexed { also_multiplexor: true, .. })
            || m.signals.iter().any(|o| o.mux_ext.iter().any(|r| r.switch == s.name))
    };
    let write = |s: &Signal, data: &mut Vec<u8>| {
        let raw = match get(s) {
            Some(v) => phys_to_raw(s, v),
            None => s.start_raw.map(|r| r as i64 as u64).unwrap_or(0),
        };
        put_bits(s, data, raw);
    };
    // switches may depend on other switches: a few passes settle them
    for _ in 0..3 {
        for s in m.signals.iter().filter(|s| is_switch(s)) {
            if signal_active(m, s, &data) {
                write(s, &mut data);
            }
        }
    }
    for s in m.signals.iter().filter(|s| !is_switch(s)) {
        if signal_active(m, s, &data) {
            write(s, &mut data);
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(start: u16, size: u16, le: bool, signed: bool) -> Signal {
        Signal {
            name: "s".into(),
            start_bit: start,
            size,
            little_endian: le,
            signed,
            value_type: ValueType::Int,
            factor: 1.0,
            offset: 0.0,
            min: 0.0,
            max: 0.0,
            unit: String::new(),
            receivers: vec![],
            mux: Mux::None,
            mux_ext: vec![],
            choices: vec![],
            comment: String::new(),
            start_raw: None,
        }
    }

    #[test]
    fn intel_and_motorola() {
        // Intel 12 bits at bit 4: data 0xF0 0xAB -> 0xABF
        let s = sig(4, 12, true, false);
        assert_eq!(raw_bits(&s, &[0xF0, 0xAB]), Some(0xABF));
        // Motorola 16 bits, MSB at bit 7 of byte 0: big endian 0x1234
        let s = sig(7, 16, false, false);
        assert_eq!(raw_bits(&s, &[0x12, 0x34]), Some(0x1234));
        // Motorola 12 bits, MSB at bit 3 of byte 0: 0x2 then 0x34 -> 0x234
        let s = sig(3, 12, false, false);
        assert_eq!(raw_bits(&s, &[0x12, 0x34]), Some(0x234));
        // signed
        let s = sig(0, 8, true, true);
        assert_eq!(decode_signal(&s, &[0xFE]), Some(-2.0));
        // out of range
        let s = sig(0, 16, true, false);
        assert_eq!(raw_bits(&s, &[0x01]), None);
    }

    #[test]
    fn fd_path_matches_fast_path() {
        let mut data = [0u8; 12];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37) ^ 0x5A;
        }
        for &le in &[true, false] {
            for start in 0..64u16 {
                for size in [1u16, 7, 12, 16, 23] {
                    let s = sig(start, size, le, false);
                    let fast = raw_bits(&s, &data[..8]);
                    let mut s2 = s.clone();
                    s2.name = "x".into();
                    let slow = raw_bits(&s2, &data);
                    if let Some(f) = fast {
                        assert_eq!(Some(f), slow, "le={le} start={start} size={size}");
                    }
                }
            }
        }
    }

    #[test]
    fn roundtrip() {
        for &le in &[true, false] {
            for &signed in &[true, false] {
                let mut s = sig(if le { 3 } else { 11 }, 13, le, signed);
                s.factor = 0.5;
                s.offset = -10.0;
                let mut d = vec![0u8; 8];
                put_bits(&s, &mut d, phys_to_raw(&s, 123.5));
                assert_eq!(decode_signal(&s, &d), Some(123.5), "le={le} signed={signed}");
            }
        }
    }
}
