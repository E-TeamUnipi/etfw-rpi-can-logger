//! On-disk layout of the raw CAN ring.
//!
//! The ring partition is an array of 4 KiB blocks. Block 0 is a superblock,
//! blocks 1..N hold data. A data block with sequence number `seq` always
//! lives at data position `seq % N` (device block `1 + seq % N`), so the
//! position of any block can be computed and a torn / foreign block is
//! detected by its CRC, ring id or a sequence number that does not match
//! its position.
//!
//! ```text
//! data block (4096 bytes)
//! +--------------------------+ 0
//! | header (96 bytes)        |
//! +--------------------------+ 96
//! | records ... (used bytes) |
//! | zero padding             |
//! +--------------------------+ 4096
//!
//! header
//!   0  magic "CLR1"          4  version u8          5  header len u8
//!   6  used u16              8  seq u64            16  session id u32
//!  20  flags u32            24  base_ts_ns u64     32  utc_offset_ns i64
//!  40  dropped u32          44  record_count u16   46  name_len u8
//!  47  reserved             48  ring_id u32        52  name [40]
//!  92  crc32 over bytes 0..92 and the used payload bytes
//!
//! record (all little endian)
//!   0 type u8   1 iface u8   2 flags u8   3 len u8   4 delta_us u32   8 payload[len]
//! ```
//!
//! Timestamps are nanoseconds of the logger's boot clock (CLOCK_BOOTTIME).
//! A record's time is `base_ts_ns + delta_us * 1000`. Wall-clock time is
//! `boot_ts + utc_offset_ns` when the TIME_VALID flag is set.

pub const BLOCK_SIZE: usize = 4096;
pub const HDR_LEN: usize = 96;
pub const PAYLOAD_LEN: usize = BLOCK_SIZE - HDR_LEN;
pub const NAME_MAX: usize = 40;

pub const BLOCK_MAGIC: [u8; 4] = *b"CLR1";
pub const FORMAT_VERSION: u8 = 1;
pub const SB_MAGIC: [u8; 8] = *b"CANRING1";

/// First block of a recording session.
pub const F_SESSION_START: u32 = 1 << 0;
/// Block was flushed because the power-fail input asserted.
pub const F_POWER_FAIL: u32 = 1 << 1;
/// `utc_offset_ns` is valid.
pub const F_TIME_VALID: u32 = 1 << 2;
/// Block contains a full copy of the session metadata (interfaces, time, name).
pub const F_CHECKPOINT: u32 = 1 << 3;
/// Time came from the on-board RTC rather than a phone/browser sync.
pub const F_TIME_FROM_RTC: u32 = 1 << 4;

/// Record types.
pub mod rt {
    /// Classic CAN frame. payload: can_id u32, dlc u8, data[..]
    pub const CAN: u8 = 0x01;
    /// CAN FD frame. payload: can_id u32, len u8, data[..]; record flags = canfd flags (BRS=1, ESI=2)
    pub const CANFD: u8 = 0x02;
    /// Logger started a session. payload: utf8 version string
    pub const SESSION: u8 = 0x10;
    /// Interface description. iface = index. payload: see `IfaceInfo::encode`
    pub const IFACE: u8 = 0x11;
    /// Wall clock sync. payload: utc_offset_ns i64, source u8
    pub const TIMESYNC: u8 = 0x12;
    /// Session name. payload: utf8
    pub const NAME: u8 = 0x13;
    /// User marker / annotation. payload: utf8
    pub const MARK: u8 = 0x14;
    /// Power-fail input changed. payload: u8 (1 = power lost, 0 = restored)
    pub const POWER: u8 = 0x15;
    /// Frames lost before reaching the ring. payload: u32 count
    pub const DROPPED: u8 = 0x16;
    /// Interface state change. iface = index. payload: u8 (see `IfState`)
    pub const IFSTATE: u8 = 0x17;
}

pub mod time_source {
    pub const UNKNOWN: u8 = 0;
    pub const HTTP: u8 = 1;
    pub const BLE: u8 = 2;
    pub const RTC: u8 = 3;
    pub const MANUAL: u8 = 4;

    pub fn name(v: u8) -> &'static str {
        match v {
            HTTP => "browser",
            BLE => "bluetooth",
            RTC => "rtc",
            MANUAL => "manual",
            _ => "unknown",
        }
    }
    pub fn parse(s: &str) -> u8 {
        match s {
            "http" | "browser" | "web" => HTTP,
            "ble" | "bluetooth" => BLE,
            "rtc" => RTC,
            "manual" => MANUAL,
            _ => UNKNOWN,
        }
    }
}

pub mod ifstate {
    pub const DOWN: u8 = 0;
    pub const UP: u8 = 1;
    pub const REMOVED: u8 = 2;
    pub const ERROR_WARNING: u8 = 3;
    pub const ERROR_PASSIVE: u8 = 4;
    pub const BUS_OFF: u8 = 5;
    pub const ERROR_ACTIVE: u8 = 6;
}

// Linux SocketCAN id flags
pub const CAN_EFF_FLAG: u32 = 0x8000_0000;
pub const CAN_RTR_FLAG: u32 = 0x4000_0000;
pub const CAN_ERR_FLAG: u32 = 0x2000_0000;
pub const CAN_SFF_MASK: u32 = 0x0000_07FF;
pub const CAN_EFF_MASK: u32 = 0x1FFF_FFFF;
pub const CANFD_BRS: u8 = 0x01;
pub const CANFD_ESI: u8 = 0x02;

#[inline]
fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
#[inline]
fn rd64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

// ---------------------------------------------------------------- superblock

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superblock {
    pub ring_id: u32,
    /// Total number of 4 KiB blocks on the device, superblock included.
    pub total_blocks: u64,
    pub created_utc_ms: i64,
}

impl Superblock {
    pub fn encode(&self, out: &mut [u8]) {
        out[..BLOCK_SIZE].fill(0);
        out[0..8].copy_from_slice(&SB_MAGIC);
        out[8..12].copy_from_slice(&(FORMAT_VERSION as u32).to_le_bytes());
        out[12..16].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
        out[16..20].copy_from_slice(&self.ring_id.to_le_bytes());
        out[24..32].copy_from_slice(&self.total_blocks.to_le_bytes());
        out[32..40].copy_from_slice(&self.created_utc_ms.to_le_bytes());
        let crc = crc32fast::hash(&out[0..60]);
        out[60..64].copy_from_slice(&crc.to_le_bytes());
    }

    pub fn decode(b: &[u8]) -> Option<Superblock> {
        if b.len() < 64 || b[0..8] != SB_MAGIC {
            return None;
        }
        if crc32fast::hash(&b[0..60]) != rd32(b, 60) {
            return None;
        }
        if rd32(b, 12) as usize != BLOCK_SIZE {
            return None;
        }
        Some(Superblock {
            ring_id: rd32(b, 16),
            total_blocks: rd64(b, 24),
            created_utc_ms: rd64(b, 32) as i64,
        })
    }

    /// Number of data blocks (N).
    pub fn data_blocks(&self) -> u64 {
        self.total_blocks.saturating_sub(1)
    }
}

// ---------------------------------------------------------------- block header

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockHeader {
    pub seq: u64,
    pub session: u32,
    pub flags: u32,
    pub used: u16,
    pub record_count: u16,
    pub base_ts_ns: u64,
    pub utc_offset_ns: i64,
    pub dropped: u32,
    pub ring_id: u32,
    pub name: String,
}

impl BlockHeader {
    pub fn time_valid(&self) -> bool {
        self.flags & F_TIME_VALID != 0
    }

    /// Write the header (without CRC) into the first HDR_LEN bytes.
    pub fn encode(&self, b: &mut [u8]) {
        b[..HDR_LEN].fill(0);
        b[0..4].copy_from_slice(&BLOCK_MAGIC);
        b[4] = FORMAT_VERSION;
        b[5] = HDR_LEN as u8;
        b[6..8].copy_from_slice(&self.used.to_le_bytes());
        b[8..16].copy_from_slice(&self.seq.to_le_bytes());
        b[16..20].copy_from_slice(&self.session.to_le_bytes());
        b[20..24].copy_from_slice(&self.flags.to_le_bytes());
        b[24..32].copy_from_slice(&self.base_ts_ns.to_le_bytes());
        b[32..40].copy_from_slice(&self.utc_offset_ns.to_le_bytes());
        b[40..44].copy_from_slice(&self.dropped.to_le_bytes());
        b[44..46].copy_from_slice(&self.record_count.to_le_bytes());
        let name = truncate_utf8(&self.name, NAME_MAX);
        b[46] = name.len() as u8;
        b[48..52].copy_from_slice(&self.ring_id.to_le_bytes());
        b[52..52 + name.len()].copy_from_slice(name.as_bytes());
    }

    /// Parse and CRC-check a block. Returns None for anything that is not a
    /// complete, valid block.
    pub fn decode(b: &[u8]) -> Option<BlockHeader> {
        if b.len() < BLOCK_SIZE || b[0..4] != BLOCK_MAGIC || b[4] != FORMAT_VERSION || b[5] as usize != HDR_LEN {
            return None;
        }
        let used = rd16(b, 6) as usize;
        if used > PAYLOAD_LEN {
            return None;
        }
        let mut h = crc32fast::Hasher::new();
        h.update(&b[0..92]);
        h.update(&b[HDR_LEN..HDR_LEN + used]);
        if h.finalize() != rd32(b, 92) {
            return None;
        }
        let name_len = (b[46] as usize).min(NAME_MAX);
        Some(BlockHeader {
            seq: rd64(b, 8),
            session: rd32(b, 16),
            flags: rd32(b, 20),
            used: used as u16,
            record_count: rd16(b, 44),
            base_ts_ns: rd64(b, 24),
            utc_offset_ns: rd64(b, 32) as i64,
            dropped: rd32(b, 40),
            ring_id: rd32(b, 48),
            name: String::from_utf8_lossy(&b[52..52 + name_len]).into_owned(),
        })
    }
}

/// Stamp sequence number and CRC into a block whose header was otherwise
/// completed by `BlockBuilder::finish`. Done by the writer right before the
/// block goes to disk, so blocks dropped in RAM never leave holes in the
/// sequence.
/// `session_base` is added to the session number the builder stored, and
/// `ring_id` is set, because only the writer knows the ring it writes to.
pub fn seal_block_for(b: &mut [u8], seq: u64, session_base: u32, ring_id: u32) {
    let s = rd32(b, 16).wrapping_add(session_base);
    b[16..20].copy_from_slice(&s.to_le_bytes());
    b[48..52].copy_from_slice(&ring_id.to_le_bytes());
    seal_block(b, seq);
}

pub fn seal_block(b: &mut [u8], seq: u64) {
    b[8..16].copy_from_slice(&seq.to_le_bytes());
    let used = rd16(b, 6) as usize;
    let mut h = crc32fast::Hasher::new();
    h.update(&b[0..92]);
    h.update(&b[HDR_LEN..HDR_LEN + used]);
    let crc = h.finalize();
    b[92..96].copy_from_slice(&crc.to_le_bytes());
}

pub fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ---------------------------------------------------------------- builder

/// Margin subtracted from the first record's timestamp to form the block
/// base, so slightly out-of-order timestamps from different interfaces still
/// encode without clamping.
const BASE_MARGIN_NS: u64 = 50_000_000;

/// Fills the payload area of one block with records.
pub struct BlockBuilder {
    used: usize,
    count: u16,
    base_ts: Option<u64>,
    pub flags: u32,
}

impl Default for BlockBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockBuilder {
    pub fn new() -> Self {
        BlockBuilder { used: 0, count: 0, base_ts: None, flags: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn used(&self) -> usize {
        self.used
    }

    /// Append a record. `buf` is the whole block buffer. Returns false if the
    /// record does not fit (caller must finish the block and retry on a new one).
    pub fn push(&mut self, buf: &mut [u8], typ: u8, iface: u8, rflags: u8, ts_ns: u64, payload: &[&[u8]]) -> bool {
        let plen: usize = payload.iter().map(|p| p.len()).sum();
        debug_assert!(plen <= 255);
        let plen = plen.min(255);
        let need = 8 + plen;
        if self.used + need > PAYLOAD_LEN {
            return false;
        }
        let base = *self.base_ts.get_or_insert(ts_ns.saturating_sub(BASE_MARGIN_NS));
        let delta_us = ts_ns.saturating_sub(base) / 1000;
        if delta_us > u32::MAX as u64 {
            return false;
        }
        let o = HDR_LEN + self.used;
        buf[o] = typ;
        buf[o + 1] = iface;
        buf[o + 2] = rflags;
        buf[o + 3] = plen as u8;
        buf[o + 4..o + 8].copy_from_slice(&(delta_us as u32).to_le_bytes());
        let mut p = o + 8;
        let mut left = plen;
        for part in payload {
            let n = part.len().min(left);
            buf[p..p + n].copy_from_slice(&part[..n]);
            p += n;
            left -= n;
        }
        self.used += need;
        self.count = self.count.saturating_add(1);
        true
    }

    /// Write the header into `buf`, zero the unused payload and reset the
    /// builder. Sequence number and CRC are added later by `seal_block`.
    pub fn finish(&mut self, buf: &mut [u8], mut hdr: BlockHeader) {
        buf[HDR_LEN + self.used..BLOCK_SIZE].fill(0);
        hdr.used = self.used as u16;
        hdr.record_count = self.count;
        hdr.base_ts_ns = self.base_ts.unwrap_or(0);
        hdr.flags |= self.flags;
        hdr.encode(buf);
        *self = BlockBuilder::new();
    }
}

// ---------------------------------------------------------------- records

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IfaceInfo {
    pub name: String,
    pub label: String,
    pub serial: String,
    pub usb_port: String,
    pub driver: String,
    pub bitrate: u32,
    pub dbitrate: u32,
    pub fd: bool,
    pub listen_only: bool,
}

impl IfaceInfo {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(64);
        v.extend_from_slice(&self.bitrate.to_le_bytes());
        v.extend_from_slice(&self.dbitrate.to_le_bytes());
        v.push((self.fd as u8) | ((self.listen_only as u8) << 1));
        for s in [&self.name, &self.label, &self.serial, &self.usb_port, &self.driver] {
            let s = truncate_utf8(s, 40);
            if v.len() + 1 + s.len() > 255 {
                v.push(0);
                continue;
            }
            v.push(s.len() as u8);
            v.extend_from_slice(s.as_bytes());
        }
        v.truncate(255);
        v
    }

    pub fn decode(p: &[u8]) -> Option<IfaceInfo> {
        if p.len() < 9 {
            return None;
        }
        let mut info = IfaceInfo {
            bitrate: rd32(p, 0),
            dbitrate: rd32(p, 4),
            fd: p[8] & 1 != 0,
            listen_only: p[8] & 2 != 0,
            ..Default::default()
        };
        let mut o = 9;
        let mut strs: Vec<String> = Vec::new();
        while o < p.len() && strs.len() < 5 {
            let n = p[o] as usize;
            o += 1;
            let end = (o + n).min(p.len());
            strs.push(String::from_utf8_lossy(&p[o..end]).into_owned());
            o = end;
        }
        let mut it = strs.into_iter();
        info.name = it.next().unwrap_or_default();
        info.label = it.next().unwrap_or_default();
        info.serial = it.next().unwrap_or_default();
        info.usb_port = it.next().unwrap_or_default();
        info.driver = it.next().unwrap_or_default();
        Some(info)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record<'a> {
    Can { iface: u8, id: u32, fd: bool, fd_flags: u8, len: u8, data: &'a [u8] },
    Session { version: &'a str },
    Iface { iface: u8, info: IfaceInfo },
    TimeSync { utc_offset_ns: i64, source: u8 },
    Name(&'a str),
    Mark(&'a str),
    Power { lost: bool },
    Dropped(u32),
    IfState { iface: u8, state: u8 },
    Unknown { typ: u8 },
}

/// Iterate over the records of a valid block, yielding (timestamp_ns, record).
pub struct RecordIter<'a> {
    buf: &'a [u8],
    pos: usize,
    end: usize,
    base: u64,
}

impl<'a> RecordIter<'a> {
    pub fn new(block: &'a [u8], hdr: &BlockHeader) -> Self {
        RecordIter { buf: block, pos: HDR_LEN, end: HDR_LEN + hdr.used as usize, base: hdr.base_ts_ns }
    }
}

fn utf8(p: &[u8]) -> &str {
    match std::str::from_utf8(p) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&p[..e.valid_up_to()]).unwrap_or(""),
    }
}

impl<'a> Iterator for RecordIter<'a> {
    type Item = (u64, Record<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos + 8 > self.end {
            return None;
        }
        let b = self.buf;
        let o = self.pos;
        let typ = b[o];
        let iface = b[o + 1];
        let rflags = b[o + 2];
        let len = b[o + 3] as usize;
        let delta = rd32(b, o + 4) as u64;
        if o + 8 + len > self.end {
            self.pos = self.end;
            return None;
        }
        let p = &b[o + 8..o + 8 + len];
        self.pos = o + 8 + len;
        let ts = self.base + delta * 1000;
        let rec = match typ {
            rt::CAN | rt::CANFD if len >= 5 => Record::Can {
                iface,
                id: rd32(p, 0),
                fd: typ == rt::CANFD,
                fd_flags: rflags,
                len: p[4],
                data: &p[5..],
            },
            rt::SESSION => Record::Session { version: utf8(p) },
            rt::IFACE => match IfaceInfo::decode(p) {
                Some(info) => Record::Iface { iface, info },
                None => Record::Unknown { typ },
            },
            rt::TIMESYNC if len >= 9 => Record::TimeSync { utc_offset_ns: rd64(p, 0) as i64, source: p[8] },
            rt::NAME => Record::Name(utf8(p)),
            rt::MARK => Record::Mark(utf8(p)),
            rt::POWER if len >= 1 => Record::Power { lost: p[0] != 0 },
            rt::DROPPED if len >= 4 => Record::Dropped(rd32(p, 0)),
            rt::IFSTATE if len >= 1 => Record::IfState { iface, state: p[0] },
            _ => Record::Unknown { typ },
        };
        Some((ts, rec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_block() {
        let mut buf = vec![0u8; BLOCK_SIZE];
        let mut b = BlockBuilder::new();
        let id = 0x123u32.to_le_bytes();
        assert!(b.push(&mut buf, rt::CAN, 0, 0, 1_000_000_000, &[&id, &[3], &[1, 2, 3]]));
        assert!(b.push(&mut buf, rt::NAME, 0, 0, 1_000_500_000, &[b"drive"]));
        let info = IfaceInfo { name: "can0".into(), label: "PT".into(), serial: "ABC".into(), bitrate: 500_000, listen_only: true, ..Default::default() };
        assert!(b.push(&mut buf, rt::IFACE, 2, 0, 1_000_600_000, &[&info.encode()]));
        b.finish(&mut buf, BlockHeader { session: 7, ring_id: 42, name: "drive".into(), ..Default::default() });
        seal_block(&mut buf, 99);
        let h = BlockHeader::decode(&buf).unwrap();
        assert_eq!(h.seq, 99);
        assert_eq!(h.session, 7);
        assert_eq!(h.record_count, 3);
        assert_eq!(h.name, "drive");
        let recs: Vec<_> = RecordIter::new(&buf, &h).collect();
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0].0, 1_000_000_000);
        assert_eq!(recs[0].1, Record::Can { iface: 0, id: 0x123, fd: false, fd_flags: 0, len: 3, data: &[1, 2, 3] });
        assert_eq!(recs[1].1, Record::Name("drive"));
        assert_eq!(recs[2].1, Record::Iface { iface: 2, info });
        // corrupt one payload byte -> CRC fails
        buf[HDR_LEN + 9] ^= 0xff;
        assert!(BlockHeader::decode(&buf).is_none());
    }

    #[test]
    fn block_fills_up() {
        let mut buf = vec![0u8; BLOCK_SIZE];
        let mut b = BlockBuilder::new();
        let mut n = 0;
        while b.push(&mut buf, rt::CAN, 0, 0, 5_000_000_000 + n * 1000, &[&[0; 4], &[8], &[0xAA; 8]]) {
            n += 1;
        }
        assert_eq!(n as usize, PAYLOAD_LEN / 21);
    }
}
