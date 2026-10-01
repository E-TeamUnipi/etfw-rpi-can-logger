//! Access to the ring device: aligned buffers, direct I/O, head search and
//! session enumeration. Readers and the writer use the same code.

use crate::format::*;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::Path;

// ---------------------------------------------------------------- aligned buffers

/// A heap buffer aligned to 4096 bytes, as required by O_DIRECT.
pub struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
}

unsafe impl Send for AlignedBuf {}
unsafe impl Sync for AlignedBuf {}

impl AlignedBuf {
    pub fn new(len: usize) -> AlignedBuf {
        assert!(len > 0 && len % BLOCK_SIZE == 0);
        let layout = std::alloc::Layout::from_size_align(len, BLOCK_SIZE).unwrap();
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        AlignedBuf { ptr, len }
    }
    pub fn block() -> AlignedBuf {
        Self::new(BLOCK_SIZE)
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        let layout = std::alloc::Layout::from_size_align(self.len, BLOCK_SIZE).unwrap();
        unsafe { std::alloc::dealloc(self.ptr, layout) }
    }
}

impl std::ops::Deref for AlignedBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}
impl std::ops::DerefMut for AlignedBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

// ---------------------------------------------------------------- block stores

/// Raw storage of data blocks, addressed by data position 0..N.
pub trait BlockStore {
    fn data_blocks(&self) -> u64;
    fn ring_id(&self) -> u32;
    /// Read `buf.len() / BLOCK_SIZE` consecutive blocks starting at `pos`.
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> io::Result<()>;
}

/// The ring on a block device (or a plain file for development).
pub struct RingDevice {
    file: File,
    pub direct: bool,
    pub total_blocks: u64,
    pub sb: Option<Superblock>,
    pub path: String,
}

const BLKGETSIZE64: libc::c_ulong = 0x8008_1272;

fn device_size(f: &File) -> io::Result<u64> {
    let md = f.metadata()?;
    if md.file_type().is_block_device() {
        let mut size: u64 = 0;
        let r = unsafe { libc::ioctl(f.as_raw_fd(), BLKGETSIZE64 as _, &mut size as *mut u64) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(size)
    } else {
        Ok(md.len())
    }
}

impl RingDevice {
    /// Open the ring. Tries O_DIRECT first (needed so readers never see stale
    /// page-cache data and writes go straight to the card), falls back to
    /// buffered I/O on filesystems that refuse it (tmpfs in development).
    pub fn open(path: &Path, write: bool) -> io::Result<RingDevice> {
        let mk = |direct: bool| {
            let mut o = OpenOptions::new();
            o.read(true).write(write);
            let mut flags = 0;
            if direct {
                flags |= libc::O_DIRECT;
            }
            if write {
                flags |= libc::O_DSYNC;
            }
            o.custom_flags(flags);
            o.open(path)
        };
        let (file, direct) = match mk(true) {
            Ok(f) => (f, true),
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => (mk(false)?, false),
            Err(e) => return Err(e),
        };
        let total_blocks = device_size(&file)? / BLOCK_SIZE as u64;
        let mut dev = RingDevice { file, direct, total_blocks, sb: None, path: path.display().to_string() };
        if total_blocks >= 2 {
            let mut b = AlignedBuf::block();
            if dev.pread(0, &mut b).is_ok() {
                dev.sb = Superblock::decode(&b).filter(|sb| sb.total_blocks == total_blocks);
            }
        }
        Ok(dev)
    }

    /// Create a (new) superblock, which invalidates every block of any
    /// previous ring on this device.
    pub fn format(&mut self, ring_id: u32, now_utc_ms: i64) -> io::Result<()> {
        if self.total_blocks < 16 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "ring device too small"));
        }
        let sb = Superblock { ring_id, total_blocks: self.total_blocks, created_utc_ms: now_utc_ms };
        let mut b = AlignedBuf::block();
        sb.encode(&mut b);
        self.pwrite_all(0, &b)?;
        self.file.sync_all()?;
        self.sb = Some(sb);
        Ok(())
    }

    fn pread(&self, dev_block: u64, buf: &mut [u8]) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        self.file.read_exact_at(buf, dev_block * BLOCK_SIZE as u64)
    }

    fn pwrite_all(&self, dev_block: u64, buf: &[u8]) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        self.file.write_all_at(buf, dev_block * BLOCK_SIZE as u64)
    }

    /// Write consecutive data blocks starting at data position `pos` with a
    /// single system call. All blocks must fit before the end of the ring.
    pub fn write_blocks(&self, pos: u64, blocks: &[&[u8]]) -> io::Result<()> {
        let n = self.data_blocks();
        assert!(pos + blocks.len() as u64 <= n);
        let mut iov: Vec<libc::iovec> = blocks
            .iter()
            .map(|b| libc::iovec { iov_base: b.as_ptr() as *mut libc::c_void, iov_len: BLOCK_SIZE })
            .collect();
        let mut off = ((1 + pos) * BLOCK_SIZE as u64) as i64;
        let mut idx = 0;
        while idx < iov.len() {
            let cnt = (iov.len() - idx).min(1024);
            let r = unsafe { libc::pwritev(self.file.as_raw_fd(), iov[idx..].as_ptr(), cnt as i32, off) };
            if r < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            let mut written = r as usize;
            if written == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "short write"));
            }
            off += written as i64;
            // advance over fully written iovecs (O_DIRECT writes whole blocks)
            while written > 0 && idx < iov.len() {
                if written >= iov[idx].iov_len {
                    written -= iov[idx].iov_len;
                    idx += 1;
                } else {
                    iov[idx].iov_base = unsafe { (iov[idx].iov_base as *mut u8).add(written) } as *mut libc::c_void;
                    iov[idx].iov_len -= written;
                    written = 0;
                }
            }
        }
        Ok(())
    }
}

impl BlockStore for RingDevice {
    fn data_blocks(&self) -> u64 {
        self.sb.as_ref().map(|s| s.data_blocks()).unwrap_or(0)
    }
    fn ring_id(&self) -> u32 {
        self.sb.as_ref().map(|s| s.ring_id).unwrap_or(0)
    }
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> io::Result<()> {
        self.pread(1 + pos, buf)
    }
}

/// In-memory store, used by tests and by the simulator.
pub struct MemStore {
    pub blocks: Vec<Vec<u8>>,
    pub ring_id: u32,
}

impl MemStore {
    pub fn new(n: usize, ring_id: u32) -> Self {
        MemStore { blocks: vec![vec![0u8; BLOCK_SIZE]; n], ring_id }
    }
}

impl BlockStore for MemStore {
    fn data_blocks(&self) -> u64 {
        self.blocks.len() as u64
    }
    fn ring_id(&self) -> u32 {
        self.ring_id
    }
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> io::Result<()> {
        for (i, chunk) in buf.chunks_mut(BLOCK_SIZE).enumerate() {
            chunk.copy_from_slice(&self.blocks[pos as usize + i]);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- scanning

/// Summary of one recording session found on the ring.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInfo {
    pub id: u32,
    pub first_seq: u64,
    pub last_seq: u64,
    pub blocks: u64,
    /// True if the first block of the session is still on the ring.
    pub complete: bool,
    pub start_ts_ns: u64,
    pub end_ts_ns: u64,
    pub name: String,
    pub time_valid: bool,
    pub time_from_rtc: bool,
    pub utc_offset_ns: i64,
    pub start_utc_ms: Option<i64>,
    pub end_utc_ms: Option<i64>,
    pub duration_s: f64,
    pub power_fail: bool,
    pub dropped: u32,
}

pub struct Ring<S: BlockStore> {
    pub store: S,
    buf: AlignedBuf,
}

/// How many blocks to probe linearly when looking for a valid neighbour.
const PROBE: u64 = 64;

impl<S: BlockStore> Ring<S> {
    pub fn new(store: S) -> Self {
        Ring { store, buf: AlignedBuf::block() }
    }

    pub fn n(&self) -> u64 {
        self.store.data_blocks()
    }

    /// Valid header at data position `pos`, or None.
    pub fn header_at(&mut self, pos: u64) -> Option<BlockHeader> {
        let n = self.n();
        if n == 0 || pos >= n {
            return None;
        }
        self.store.read_at(pos, &mut self.buf).ok()?;
        let h = BlockHeader::decode(&self.buf)?;
        (h.ring_id == self.store.ring_id() && h.seq % n == pos).then_some(h)
    }

    /// Valid header of the block with sequence number `seq`, or None if that
    /// block is torn, missing, or has been overwritten.
    pub fn header_seq(&mut self, seq: u64) -> Option<BlockHeader> {
        let n = self.n();
        if n == 0 {
            return None;
        }
        self.header_at(seq % n).filter(|h| h.seq == seq)
    }

    /// Full block (header + buffer copy) by sequence number.
    pub fn block_seq(&mut self, seq: u64) -> Option<(BlockHeader, Vec<u8>)> {
        let h = self.header_seq(seq)?;
        Some((h, self.buf.to_vec()))
    }

    /// First valid block with seq in [from, to].
    fn next_valid(&mut self, from: u64, to: u64) -> Option<BlockHeader> {
        let mut s = from;
        while s <= to {
            if let Some(h) = self.header_seq(s) {
                return Some(h);
            }
            s += 1;
        }
        None
    }

    /// Find the newest valid block. O(log N) reads.
    pub fn find_head(&mut self) -> Option<BlockHeader> {
        let n = self.n();
        if n == 0 {
            return None;
        }
        // reference: first valid block near the start of the device
        let mut reference = None;
        for p in 0..PROBE.min(n) {
            if let Some(h) = self.header_at(p) {
                reference = Some((p, h));
                break;
            }
        }
        let (rpos, rh) = reference?;
        let lap = rh.seq / n;
        // Positions [rpos..=head] belong to `lap`, later ones to an older lap
        // (or were never written). Binary search the last position of `lap`.
        let (mut lo, mut hi) = (rpos, n - 1);
        let mut best = rh;
        while lo < hi {
            let mid = lo + (hi - lo + 1) / 2;
            // probe mid, stepping over a few invalid blocks if needed
            let mut found = None;
            let mut p = mid;
            while p <= hi && p < mid + PROBE {
                if let Some(h) = self.header_at(p) {
                    found = Some((p, h));
                    break;
                }
                p += 1;
            }
            match found {
                Some((p, h)) if h.seq / n == lap => {
                    lo = p;
                    best = h;
                }
                _ => hi = mid - 1,
            }
        }
        // Guard against holes left by write errors: look a little further.
        let mut p = best.seq % n + 1;
        let end = (p + PROBE).min(n);
        while p < end {
            if let Some(h) = self.header_at(p) {
                if h.seq / n == lap && h.seq > best.seq {
                    best = h;
                }
            }
            p += 1;
        }
        Some(best)
    }

    /// Oldest sequence number that can still be on the ring given the head.
    pub fn oldest_possible(&self, head_seq: u64) -> u64 {
        (head_seq + 1).saturating_sub(self.n())
    }

    /// Enumerate sessions, oldest first. Uses binary search on the session id
    /// (which only grows), so it reads O(sessions * log N) blocks.
    pub fn sessions(&mut self) -> Vec<SessionInfo> {
        let mut out = Vec::new();
        let head = match self.find_head() {
            Some(h) => h,
            None => return out,
        };
        let hi = head.seq;
        let lo = self.oldest_possible(hi);
        let mut cur = self.next_valid(lo, hi);
        while let Some(first) = cur {
            let sid = first.session;
            // binary search the last seq with this session id
            let (mut a, mut b) = (first.seq, hi);
            let mut last = first.clone();
            while a < b {
                let mid = a + (b - a + 1) / 2;
                match self.next_valid(mid, (mid + PROBE).min(b)) {
                    Some(h) if h.session == sid => {
                        a = h.seq;
                        last = h;
                    }
                    Some(h) if h.session < sid => {
                        // should not happen (ids only grow); be defensive
                        a = h.seq;
                    }
                    _ => b = mid - 1,
                }
            }
            // end timestamp: last record of the last block
            let mut end_ts = last.base_ts_ns;
            if let Some((h, blk)) = self.block_seq(last.seq) {
                if let Some((ts, _)) = RecordIter::new(&blk, &h).last() {
                    end_ts = ts;
                }
            }
            let mut start_ts = first.base_ts_ns;
            if let Some((h, blk)) = self.block_seq(first.seq) {
                if let Some((ts, _)) = RecordIter::new(&blk, &h).next() {
                    start_ts = ts;
                }
            }
            let tv = last.time_valid();
            let to_utc = |ts: u64| -> Option<i64> { tv.then(|| (ts as i64 + last.utc_offset_ns) / 1_000_000) };
            out.push(SessionInfo {
                id: sid,
                first_seq: first.seq,
                last_seq: last.seq,
                blocks: last.seq - first.seq + 1,
                complete: first.flags & F_SESSION_START != 0,
                start_ts_ns: start_ts,
                end_ts_ns: end_ts,
                name: last.name.clone(),
                time_valid: tv,
                time_from_rtc: last.flags & F_TIME_FROM_RTC != 0,
                utc_offset_ns: last.utc_offset_ns,
                start_utc_ms: to_utc(start_ts),
                end_utc_ms: to_utc(end_ts),
                duration_s: end_ts.saturating_sub(start_ts) as f64 / 1e9,
                power_fail: last.flags & F_POWER_FAIL != 0,
                dropped: last.dropped,
            });
            cur = if last.seq < hi { self.next_valid(last.seq + 1, hi) } else { None };
        }
        out
    }

    /// Visit the blocks of [first, last] in order. Reads in large chunks.
    /// Stops early (returning Ok(false)) if the writer overwrote the region
    /// being read (only possible for the oldest data on a full ring).
    pub fn for_each_block<F>(&mut self, first: u64, last: u64, mut f: F) -> io::Result<bool>
    where
        F: FnMut(&BlockHeader, &[u8]) -> io::Result<bool>,
    {
        let n = self.n();
        if n == 0 {
            return Ok(true);
        }
        const CHUNK: u64 = 256;
        let mut big = AlignedBuf::new(CHUNK as usize * BLOCK_SIZE);
        let rid = self.store.ring_id();
        let mut seq = first;
        while seq <= last {
            let pos = seq % n;
            let cnt = CHUNK.min(last - seq + 1).min(n - pos);
            let bytes = cnt as usize * BLOCK_SIZE;
            self.store.read_at(pos, &mut big[..bytes])?;
            for i in 0..cnt as usize {
                let blk = &big[i * BLOCK_SIZE..(i + 1) * BLOCK_SIZE];
                let expect = seq + i as u64;
                match BlockHeader::decode(blk) {
                    Some(h) if h.ring_id == rid && h.seq == expect => {
                        if !f(&h, blk)? {
                            return Ok(true);
                        }
                    }
                    Some(h) if h.ring_id == rid && h.seq > expect => return Ok(false),
                    _ => {} // torn or missing block: skip
                }
            }
            seq += cnt;
        }
        Ok(true)
    }
}

/// Random 32-bit id from the kernel.
pub fn random_u32() -> u32 {
    let mut b = [0u8; 4];
    let r = unsafe { libc::getrandom(b.as_mut_ptr() as *mut libc::c_void, 4, 0) };
    if r != 4 {
        // extremely unlikely; fall back to the clock
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        return t.subsec_nanos() ^ (t.as_secs() as u32);
    }
    u32::from_le_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(store: &mut MemStore, seq: u64, session: u32, ts: u64, flags: u32) {
        let n = store.blocks.len() as u64;
        let mut buf = vec![0u8; BLOCK_SIZE];
        let mut b = BlockBuilder::new();
        b.push(&mut buf, rt::CAN, 0, 0, ts, &[&1u32.to_le_bytes(), &[1], &[seq as u8]]);
        b.finish(&mut buf, BlockHeader { session, flags, ring_id: store.ring_id, name: format!("s{session}"), ..Default::default() });
        seal_block(&mut buf, seq);
        store.blocks[(seq % n) as usize] = buf;
    }

    /// Write `count` blocks starting at seq 0, new session every `per` blocks.
    fn fill(n: usize, count: u64, per: u64) -> MemStore {
        let mut s = MemStore::new(n, 77);
        for seq in 0..count {
            let sess = (seq / per) as u32 + 1;
            let flags = if seq % per == 0 { F_SESSION_START } else { 0 };
            put(&mut s, seq, sess, 1_000_000_000 + seq * 1_000_000, flags);
        }
        s
    }

    #[test]
    fn empty_ring() {
        let mut r = Ring::new(MemStore::new(100, 1));
        assert!(r.find_head().is_none());
        assert!(r.sessions().is_empty());
    }

    #[test]
    fn head_no_wrap_and_wrapped() {
        for n in [16usize, 100, 1000] {
            for count in [1u64, 2, 15, 16, 17, 99, 100, 101, 250, 999, 1000, 1001, 2500] {
                let mut r = Ring::new(fill(n, count, 7));
                let h = r.find_head().unwrap();
                assert_eq!(h.seq, count - 1, "n={n} count={count}");
            }
        }
    }

    #[test]
    fn torn_blocks() {
        let n = 200usize;
        // torn block right after head
        let mut s = fill(n, 530, 50);
        s.blocks[530 % n][100] ^= 1; // garbage at next position (old lap) -> still fine
        s.blocks[(530 % n) + 1][20] ^= 1;
        let mut r = Ring::new(s);
        assert_eq!(r.find_head().unwrap().seq, 529);

        // block 0 torn after wrap: head is last block of previous lap
        let mut s = fill(n, 400, 50);
        s.blocks[0][30] ^= 1; // seq 400 would go here; pretend it was torn
        let mut r = Ring::new(s);
        assert_eq!(r.find_head().unwrap().seq, 399);

        // a hole (write error) in the middle of the current lap
        let mut s = fill(n, 350, 50);
        s.blocks[120][30] ^= 1;
        let mut r = Ring::new(s);
        assert_eq!(r.find_head().unwrap().seq, 349);
    }

    #[test]
    fn foreign_blocks_ignored() {
        let mut s = fill(100, 30, 10);
        s.ring_id = 5; // pretend the superblock was re-created
        let mut r = Ring::new(s);
        assert!(r.find_head().is_none());
    }

    #[test]
    fn sessions_enumerated() {
        let mut r = Ring::new(fill(100, 95, 10));
        let ss = r.sessions();
        assert_eq!(ss.len(), 10);
        assert_eq!(ss[0].id, 1);
        assert_eq!(ss[0].first_seq, 0);
        assert_eq!(ss[0].last_seq, 9);
        assert!(ss[0].complete);
        assert_eq!(ss[9].first_seq, 90);
        assert_eq!(ss[9].last_seq, 94);
        assert_eq!(ss[3].name, "s4");

        // wrapped: the oldest session is partially overwritten
        let mut r = Ring::new(fill(100, 235, 10));
        let ss = r.sessions();
        assert_eq!(ss.first().unwrap().first_seq, 135);
        assert!(!ss.first().unwrap().complete);
        assert_eq!(ss.first().unwrap().id, 14);
        assert_eq!(ss.last().unwrap().last_seq, 234);
        let total: u64 = ss.iter().map(|s| s.blocks).sum();
        assert_eq!(total, 100);
    }

    #[test]
    fn iterate_blocks() {
        let mut r = Ring::new(fill(64, 300, 1000));
        let mut seen = vec![];
        r.for_each_block(236, 299, |h, _| {
            seen.push(h.seq);
            Ok(true)
        })
        .unwrap();
        assert_eq!(seen, (236..300).collect::<Vec<_>>());
    }
}
