//! Block pipeline (RAM queue) and the writer thread that puts blocks on the
//! ring with direct, synchronous I/O.

use crate::Shared;
use canlog_core::format::*;
use canlog_core::ring::{random_u32, AlignedBuf, Ring, RingDevice};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::Ordering::*;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub struct Queued {
    pub buf: AlignedBuf,
    /// Last block before a power-fail hold.
    pub pf: bool,
}

/// Finished blocks waiting for the writer, plus a free list. The number of
/// buffers is capped; if the writer cannot keep up (or is holding during a
/// power dip) the oldest queued block is recycled and counted as lost.
pub struct Pipeline {
    free: Mutex<Vec<AlignedBuf>>,
    queue: Mutex<VecDeque<Queued>>,
    cv: Condvar,
    allocated: Mutex<usize>,
    cap: usize,
}

impl Pipeline {
    pub fn new(cap_blocks: usize) -> Pipeline {
        Pipeline {
            free: Mutex::new(Vec::new()),
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            allocated: Mutex::new(0),
            cap: cap_blocks.max(16),
        }
    }

    pub fn get_buf(&self, shared: &Shared) -> AlignedBuf {
        if let Some(b) = self.free.lock().unwrap().pop() {
            return b;
        }
        {
            let mut a = self.allocated.lock().unwrap();
            if *a < self.cap {
                *a += 1;
                return AlignedBuf::block();
            }
        }
        // RAM buffer full: sacrifice the oldest queued block
        if let Some(q) = self.queue.lock().unwrap().pop_front() {
            shared.lost_blocks.fetch_add(1, Relaxed);
            return q.buf;
        }
        // the writer holds every buffer right now; allocate one more
        AlignedBuf::block()
    }

    pub fn submit(&self, buf: AlignedBuf, pf: bool) {
        self.queue.lock().unwrap().push_back(Queued { buf, pf });
        self.cv.notify_one();
    }

    pub fn recycle(&self, buf: AlignedBuf) {
        self.free.lock().unwrap().push(buf);
    }

    pub fn queued(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    pub fn notify(&self) {
        self.cv.notify_all();
    }
}

pub const ST_STARTING: u8 = 0;
pub const ST_RECORDING: u8 = 1;
pub const ST_POWER_HOLD: u8 = 2;
pub const ST_NO_STORAGE: u8 = 3;
pub const ST_STOPPED: u8 = 4;

pub fn state_name(s: u8) -> &'static str {
    match s {
        ST_STARTING => "starting",
        ST_RECORDING => "recording",
        ST_POWER_HOLD => "power_hold",
        ST_NO_STORAGE => "no_storage",
        _ => "stopped",
    }
}

struct Target {
    dev: RingDevice,
    next_seq: u64,
    session_base: u32,
    ring_id: u32,
    n: u64,
}

fn open_target(path: &PathBuf, shared: &Shared) -> std::io::Result<Target> {
    let mut dev = RingDevice::open(path, true)?;
    if dev.sb.is_none() {
        eprintln!("canlogd: no valid ring on {}, formatting", path.display());
        dev.format(random_u32(), canlog_core::realtime_ns() / 1_000_000)?;
    }
    if !dev.direct {
        eprintln!("canlogd: warning: O_DIRECT not supported on {}, using buffered writes", path.display());
    }
    let t0 = Instant::now();
    let mut ring = Ring::new(dev);
    let head = ring.find_head();
    let n = ring.n();
    let ring_id = ring.store.sb.as_ref().unwrap().ring_id;
    let (next_seq, session_base) = match &head {
        Some(h) => (h.seq + 1, h.session + 1),
        None => (0, 1),
    };
    eprintln!(
        "canlogd: ring {} blocks, head {:?}, session {} (scan {:?})",
        n,
        head.as_ref().map(|h| h.seq),
        session_base,
        t0.elapsed()
    );
    shared.ring_blocks.store(n, Relaxed);
    shared.session_base.store(session_base, Relaxed);
    shared.head_seq.store(next_seq, Relaxed);
    Ok(Target { dev: ring.store, next_seq, session_base, ring_id, n })
}

/// Writer thread main loop.
pub fn run(path: PathBuf, pipe: Arc<Pipeline>, shared: Arc<Shared>) {
    let mut target: Option<Target> = None;
    let mut last_open_try = Instant::now() - Duration::from_secs(60);
    let mut pf_flushed = false;
    let mut pf_since: Option<Instant> = None;
    let mut consecutive_errors = 0u32;
    const BATCH: usize = 64;

    loop {
        shared.writer_heartbeat.store(canlog_core::boot_ns(), Relaxed);
        let stopping = shared.stop.load(Relaxed);

        if target.is_none() {
            if last_open_try.elapsed() >= Duration::from_secs(2) || stopping {
                last_open_try = Instant::now();
                match open_target(&path, &shared) {
                    Ok(t) => {
                        target = Some(t);
                        consecutive_errors = 0;
                    }
                    Err(e) => {
                        if shared.state.load(Relaxed) != ST_NO_STORAGE {
                            eprintln!("canlogd: cannot open ring {}: {e} (buffering in RAM)", path.display());
                        }
                        shared.state.store(ST_NO_STORAGE, Relaxed);
                        if stopping {
                            break;
                        }
                    }
                }
            }
            if target.is_none() {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        }

        // power-fail handling: flush up to the power-fail block, then hold
        let pf = shared.pf_asserted.load(Relaxed);
        if !pf {
            pf_flushed = false;
            pf_since = None;
        }
        let mut limit = usize::MAX;
        if pf {
            let since = *pf_since.get_or_insert_with(Instant::now);
            if pf_flushed {
                shared.state.store(ST_POWER_HOLD, Relaxed);
                let q = pipe.queue.lock().unwrap();
                let _ = pipe.cv.wait_timeout(q, Duration::from_millis(100)).unwrap();
                continue;
            }
            let q = pipe.queue.lock().unwrap();
            match q.iter().position(|b| b.pf) {
                Some(i) => limit = i + 1,
                None if since.elapsed() > Duration::from_millis(300) => {
                    pf_flushed = true;
                    continue;
                }
                None => {}
            }
        }

        // collect a batch of consecutive positions
        let t = target.as_mut().unwrap();
        let mut batch: Vec<Queued> = Vec::with_capacity(BATCH);
        {
            let mut q = pipe.queue.lock().unwrap();
            if q.is_empty() {
                if stopping {
                    break;
                }
                let _ = pipe.cv.wait_timeout(q, Duration::from_millis(250)).unwrap();
                continue;
            }
            let room = (t.n - t.next_seq % t.n) as usize;
            let take = q.len().min(BATCH).min(room).min(limit);
            batch.extend(q.drain(..take));
        }
        let hit_pf = batch.iter().any(|b| b.pf);

        let pos = t.next_seq % t.n;
        for (i, b) in batch.iter_mut().enumerate() {
            seal_block_for(&mut b.buf, t.next_seq + i as u64, t.session_base, t.ring_id);
        }
        let mut ok = false;
        for attempt in 0..3 {
            let refs: Vec<&[u8]> = batch.iter().map(|b| &b.buf[..]).collect();
            match t.dev.write_blocks(pos, &refs) {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(e) => {
                    shared.write_errors.fetch_add(1, Relaxed);
                    eprintln!("canlogd: write error at block {pos} (attempt {}): {e}", attempt + 1);
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        let count = batch.len() as u64;
        // Sequence numbers advance even on failure: the positions are skipped
        // (readers see a hole) rather than retried forever on a bad sector.
        t.next_seq += count;
        shared.head_seq.store(t.next_seq, Relaxed);
        if ok {
            consecutive_errors = 0;
            shared.written_blocks.fetch_add(count, Relaxed);
            if !pf {
                shared.state.store(ST_RECORDING, Relaxed);
            }
        } else {
            consecutive_errors += 1;
            shared.lost_blocks.fetch_add(count, Relaxed);
            if consecutive_errors >= 5 {
                eprintln!("canlogd: ring device keeps failing, reopening");
                shared.state.store(ST_NO_STORAGE, Relaxed);
                target = None;
            }
        }
        for b in batch {
            pipe.recycle(b.buf);
        }
        if hit_pf {
            pf_flushed = true;
        }
    }
    shared.state.store(ST_STOPPED, Relaxed);
    shared.writer_done.store(true, Relaxed);
}
