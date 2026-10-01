//! Frame length on the wire, in bits, including stuff bits and the 3-bit
//! interframe space. Classic CAN is exact (the CRC is computed, so stuffing
//! is counted bit by bit). CAN FD is exact for the dynamic stuffing of the
//! header and payload; the CRC field uses fixed stuff bits by design.

/// Bits sent at the nominal rate and at the data rate (CAN FD with BRS).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bits {
    pub nominal: u32,
    pub data: u32,
}

impl Bits {
    pub fn duration_ns(&self, bitrate: u32, dbitrate: u32) -> f64 {
        if bitrate == 0 {
            return 0.0;
        }
        let dr = if dbitrate > 0 { dbitrate } else { bitrate };
        self.nominal as f64 * 1e9 / bitrate as f64 + self.data as f64 * 1e9 / dr as f64
    }
    pub fn total(&self) -> u32 {
        self.nominal + self.data
    }
}

/// What a frame looks like on the wire.
#[derive(Debug, Clone, Copy, Default)]
pub struct Shape {
    pub id: u32,
    pub ext: bool,
    pub rtr: bool,
    pub fd: bool,
    pub brs: bool,
}

struct BitBuf {
    v: Vec<bool>,
}

impl BitBuf {
    fn push(&mut self, val: u32, n: u32) {
        for i in (0..n).rev() {
            self.v.push(val >> i & 1 == 1);
        }
    }
}

/// Number of dynamic stuff bits for `bits` (a stuff bit after 5 equal bits;
/// the stuff bit itself starts the next run).
fn stuff_count(bits: &[bool], positions: &mut Vec<usize>) -> u32 {
    let (mut last, mut run, mut n) = (None, 0u32, 0u32);
    positions.clear();
    for (i, &b) in bits.iter().enumerate() {
        if Some(b) == last {
            run += 1;
        } else {
            last = Some(b);
            run = 1;
        }
        if run == 5 {
            n += 1;
            positions.push(i);
            last = Some(!b);
            run = 1;
        }
    }
    n
}

fn crc15(bits: &[bool]) -> u32 {
    let mut crc: u32 = 0;
    for &b in bits {
        let next = b ^ (crc >> 14 & 1 == 1);
        crc = (crc << 1) & 0x7FFF;
        if next {
            crc ^= 0x4599;
        }
    }
    crc
}

/// DLC code for a CAN FD payload length.
pub fn fd_dlc(len: usize) -> u32 {
    match len {
        0..=8 => len as u32,
        9..=12 => 9,
        13..=16 => 10,
        17..=20 => 11,
        21..=24 => 12,
        25..=32 => 13,
        33..=48 => 14,
        _ => 15,
    }
}

/// Exact bits of a frame with this payload.
pub fn frame_bits(f: &Shape, data: &[u8]) -> Bits {
    let mut b = BitBuf { v: Vec::with_capacity(64 + data.len() * 8 + 32) };
    let mut pos = Vec::new();
    b.push(0, 1); // SOF
    if f.ext {
        b.push(f.id >> 18 & 0x7FF, 11);
        b.push(1, 1); // SRR
        b.push(1, 1); // IDE
        b.push(f.id & 0x3FFFF, 18);
    } else {
        b.push(f.id & 0x7FF, 11);
    }
    if !f.fd {
        b.push(f.rtr as u32, 1);
        if f.ext {
            b.push(0, 2); // r1 r0
        } else {
            b.push(0, 2); // IDE r0
        }
        b.push(data.len().min(15) as u32, 4);
        if !f.rtr {
            for &x in data {
                b.push(x as u32, 8);
            }
        }
        let crc = crc15(&b.v);
        b.push(crc, 15);
        let stuff = stuff_count(&b.v, &mut pos);
        // CRC delimiter, ACK slot, ACK delimiter, EOF (7), IFS (3)
        return Bits { nominal: b.v.len() as u32 + stuff + 13, data: 0 };
    }
    // CAN FD
    b.push(0, 1); // RRS
    if !f.ext {
        b.push(0, 1); // IDE
    }
    b.push(1, 1); // FDF
    b.push(0, 1); // res
    b.push(f.brs as u32, 1);
    let arb = b.v.len();
    b.push(0, 1); // ESI (error active)
    b.push(fd_dlc(data.len()), 4);
    for &x in data {
        b.push(x as u32, 8);
    }
    stuff_count(&b.v, &mut pos);
    let stuff_arb = pos.iter().filter(|&&p| p < arb).count() as u32;
    let stuff_data = pos.len() as u32 - stuff_arb;
    let crc_len: u32 = if data.len() <= 16 { 17 } else { 21 };
    // stuff count (4) + CRC, a fixed stuff bit before every 4 bits
    let fsb = (4 + crc_len).div_ceil(4);
    let data_phase = (b.v.len() - arb) as u32 + stuff_data + 4 + crc_len + fsb + 1; // + CRC delimiter
    let nominal = arb as u32 + stuff_arb + 12; // ACK, ACK delim, EOF, IFS
    if f.brs {
        Bits { nominal, data: data_phase }
    } else {
        Bits { nominal: nominal + data_phase, data: 0 }
    }
}

/// Worst-case bits for any payload of `len` bytes (maximum stuffing).
pub fn worst_bits(f: &Shape, len: usize) -> Bits {
    if !f.fd {
        let g = if f.ext { 54 } else { 34 };
        let s = if f.rtr { 0 } else { len.min(8) as u32 * 8 };
        // Davis et al. 2007: g + 8s + 13 + floor((g + 8s - 1) / 4)
        return Bits { nominal: g + s + 13 + (g + s - 1) / 4, data: 0 };
    }
    let arb: u32 = if f.ext { 36 } else { 17 };
    let dyn_data = 5 + 8 * len as u32;
    let crc_len: u32 = if len <= 16 { 17 } else { 21 };
    let fsb = (4 + crc_len).div_ceil(4);
    let fixed_data = dyn_data + 4 + crc_len + fsb + 1;
    if !f.brs {
        let stuff = (arb + dyn_data - 1) / 4;
        return Bits { nominal: arb + 12 + fixed_data + stuff, data: 0 };
    }
    // per phase: a run may start in the header and finish in the payload,
    // so the payload can have a stuff bit at its very first bit
    let stuff_arb = (arb - 1) / 4;
    let stuff_data = (dyn_data + 3) / 4;
    Bits { nominal: arb + stuff_arb + 12, data: fixed_data + stuff_data }
}

/// Mean and maximum bits over random payloads (deterministic PRNG), for
/// messages whose real payloads are unknown.
pub fn random_bits(f: &Shape, len: usize, samples: u32) -> (f64, Bits) {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (f.id as u64) << 8 ^ len as u64);
    let mut data = vec![0u8; len];
    let (mut sum, mut max) = (0.0, Bits::default());
    for _ in 0..samples.max(1) {
        for x in data.iter_mut() {
            *x = rng.next() as u8;
        }
        let b = frame_bits(f, &data);
        sum += b.total() as f64;
        if b.total() > max.total() {
            max = b;
        }
    }
    (sum / samples.max(1) as f64, max)
}

/// xorshift64*: small, deterministic, good enough for payload sampling.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_case_matches_textbook() {
        // 8-byte standard frame: 135 bits; extended: 160 bits
        let s = Shape { id: 0x123, ..Default::default() };
        assert_eq!(worst_bits(&s, 8).total(), 135);
        let e = Shape { ext: true, ..s };
        assert_eq!(worst_bits(&e, 8).total(), 160);
        assert_eq!(worst_bits(&s, 0).total(), 55);
    }

    #[test]
    fn exact_never_exceeds_worst() {
        let mut rng = Rng(1);
        for ext in [false, true] {
            for fd in [false, true] {
                for len in [0usize, 1, 3, 8, 12, 20, 64] {
                    if !fd && len > 8 {
                        continue;
                    }
                    let s = Shape { id: (rng.next() & 0x7FF) as u32, ext, fd, brs: fd, rtr: false };
                    let w = worst_bits(&s, len);
                    for _ in 0..200 {
                        let d: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
                        let b = frame_bits(&s, &d);
                        assert!(b.nominal <= w.nominal && b.data <= w.data, "{s:?} len {len}: {b:?} > {w:?}");
                    }
                    // all-zero payload stuffs heavily
                    let z = frame_bits(&s, &vec![0u8; len]);
                    assert!(z.total() <= w.total());
                }
            }
        }
    }

    #[test]
    fn known_frame() {
        // id 0x000, 8 bytes of 0x00: no-stuff length 111, many stuff bits
        let s = Shape::default();
        let b = frame_bits(&s, &[0; 8]);
        assert_eq!(b.data, 0);
        assert!(b.nominal > 111 && b.nominal <= 135);
        // 0x55 pattern on id 0x555 has no runs of 5 except at the CRC
        let s = Shape { id: 0x2AA, ..Default::default() };
        let b = frame_bits(&s, &[0x55; 8]);
        assert!(b.nominal >= 111 && b.nominal < 118, "{b:?}");
    }
}
