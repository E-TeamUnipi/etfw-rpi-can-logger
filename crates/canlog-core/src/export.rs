//! Convert a session on the ring to candump `.log` or Vector `.asc` text.

use crate::format::*;
use crate::ring::{BlockStore, Ring, SessionInfo};
use crate::timefmt;
use std::collections::HashMap;
use std::io::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// can-utils candump log (`candump -l`), readable by canplayer, python-can, SavvyCAN, ...
    Candump,
    /// Vector ASCII log, readable by CANalyzer/CANoe, python-can, asammdf, ...
    Asc,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "candump" | "log" => Some(Format::Candump),
            "asc" => Some(Format::Asc),
            _ => None,
        }
    }
    pub fn ext(&self) -> &'static str {
        match self {
            Format::Candump => "log",
            Format::Asc => "asc",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Only frames at or after this many seconds from the session start.
    pub from_s: Option<f64>,
    /// Only frames before this many seconds from the session start.
    pub to_s: Option<f64>,
    /// Interfaces to include (by name). Empty = all.
    pub ifaces: Vec<String>,
}

/// Suggested download file name for a session.
pub fn file_name(s: &SessionInfo, fmt: Format) -> String {
    let when = match s.start_utc_ms {
        Some(ms) => timefmt::file_stamp(ms),
        None => format!("session{:05}", s.id),
    };
    let name: String = s
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let name = name.trim_matches('_');
    if name.is_empty() {
        format!("{when}.{}", fmt.ext())
    } else {
        format!("{when}_{name}.{}", fmt.ext())
    }
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn push_hex(out: &mut Vec<u8>, v: u32, digits: usize) {
    for i in (0..digits).rev() {
        out.push(HEX[((v >> (i * 4)) & 0xF) as usize]);
    }
}

struct Ctx {
    names: HashMap<u8, String>,
    offset_ns: Option<i64>,
    start_ts: u64,
}

impl Ctx {
    fn iface_name(&self, idx: u8) -> String {
        self.names.get(&idx).cloned().unwrap_or_else(|| format!("can{idx}"))
    }
}

/// Collect interface names from the beginning of the session. If the start
/// of the session was overwritten, the logger's periodic checkpoints (every
/// 30 s) still carry them, so we scan forward until the first checkpoint.
fn collect_ifaces<S: BlockStore>(ring: &mut Ring<S>, s: &SessionInfo) -> io::Result<HashMap<u8, String>> {
    let mut names = HashMap::new();
    let last = s.last_seq;
    ring.for_each_block(s.first_seq, last, |h, blk| {
        for (_, r) in RecordIter::new(blk, h) {
            if let Record::Iface { iface, info } = r {
                names.entry(iface).or_insert(info.name);
            }
        }
        Ok(h.flags & F_CHECKPOINT == 0)
    })?;
    Ok(names)
}

/// Write the session to `out`. Returns the number of frames written.
pub fn export<S: BlockStore, W: Write>(ring: &mut Ring<S>, s: &SessionInfo, fmt: Format, opt: &Options, out: &mut W) -> io::Result<u64> {
    let ctx = Ctx {
        names: collect_ifaces(ring, s)?,
        offset_ns: s.time_valid.then_some(s.utc_offset_ns),
        start_ts: s.start_ts_ns,
    };
    let from_ts = opt.from_s.map(|v| s.start_ts_ns + (v.max(0.0) * 1e9) as u64);
    let to_ts = opt.to_s.map(|v| s.start_ts_ns + (v.max(0.0) * 1e9) as u64);
    let wanted = |idx: u8| opt.ifaces.is_empty() || opt.ifaces.iter().any(|n| *n == ctx.iface_name(idx));

    // ASC channel numbers: stable 1-based numbering by interface index
    let mut line: Vec<u8> = Vec::with_capacity(256);
    let mut frames = 0u64;
    let mut buf: Vec<u8> = Vec::with_capacity(1 << 16);
    let mut asc_first_ts: Option<u64> = None;

    if fmt == Format::Asc {
        let start_ms = ctx.offset_ns.map(|o| (from_ts.unwrap_or(ctx.start_ts) as i64 + o) / 1_000_000).unwrap_or(0);
        let d = timefmt::asc_date(start_ms);
        write!(buf, "date {d}\nbase hex  timestamps absolute\ninternal events logged\n// version 13.0.0\n")?;
        if !s.name.is_empty() {
            writeln!(buf, "// session: {}", s.name)?;
        }
        let mut chans: Vec<_> = ctx.names.iter().collect();
        chans.sort();
        for (idx, name) in chans {
            writeln!(buf, "// channel {} = {}", *idx as u32 + 1, name)?;
        }
        if ctx.offset_ns.is_none() {
            writeln!(buf, "// note: no time sync for this session, date is a placeholder")?;
        }
        write!(buf, "Begin Triggerblock {d}\n   0.000000 Start of measurement\n")?;
    }

    let complete = ring.for_each_block(s.first_seq, s.last_seq, |h, blk| {
        for (ts, rec) in RecordIter::new(blk, h) {
            if from_ts.is_some_and(|f| ts < f) {
                continue;
            }
            if to_ts.is_some_and(|t| ts >= t) {
                return Ok(false);
            }
            line.clear();
            match (&rec, fmt) {
                (Record::Can { iface, id, fd, fd_flags, len, data }, Format::Candump) => {
                    if !wanted(*iface) {
                        continue;
                    }
                    // (seconds.micros) iface id#data
                    let t = match ctx.offset_ns {
                        Some(o) => ts as i64 + o,
                        None => ts as i64,
                    };
                    let (sec, us) = (t.div_euclid(1_000_000_000), t.rem_euclid(1_000_000_000) / 1000);
                    write!(line, "({sec}.{us:06}) {} ", ctx.iface_name(*iface))?;
                    if id & CAN_ERR_FLAG != 0 {
                        push_hex(&mut line, id & (CAN_ERR_FLAG | CAN_EFF_MASK), 8);
                    } else if id & CAN_EFF_FLAG != 0 {
                        push_hex(&mut line, id & CAN_EFF_MASK, 8);
                    } else {
                        push_hex(&mut line, id & CAN_SFF_MASK, 3);
                    }
                    line.push(b'#');
                    if *fd {
                        line.push(b'#');
                        push_hex(&mut line, *fd_flags as u32 & 0xF, 1);
                    }
                    if !*fd && id & CAN_RTR_FLAG != 0 {
                        line.push(b'R');
                        if *len > 0 && *len <= 8 {
                            push_hex(&mut line, *len as u32, 1);
                        }
                    } else {
                        for b in data.iter() {
                            push_hex(&mut line, *b as u32, 2);
                        }
                    }
                    line.push(b'\n');
                    frames += 1;
                }
                (Record::Can { iface, id, fd, fd_flags, len, data }, Format::Asc) => {
                    if !wanted(*iface) {
                        continue;
                    }
                    let first = *asc_first_ts.get_or_insert(from_ts.unwrap_or(ctx.start_ts).min(ts));
                    let rel = ts.saturating_sub(first) as f64 / 1e9;
                    let ch = *iface as u32 + 1;
                    let ext = id & CAN_EFF_FLAG != 0;
                    let raw = if ext { id & CAN_EFF_MASK } else { id & CAN_SFF_MASK };
                    let mut idtxt = Vec::with_capacity(10);
                    push_hex(&mut idtxt, raw, if ext { 8 } else { 3 });
                    let mut idtxt = String::from_utf8(idtxt).unwrap().trim_start_matches('0').to_string();
                    if idtxt.is_empty() {
                        idtxt.push('0');
                    }
                    if ext {
                        idtxt.push('x');
                    }
                    if id & CAN_ERR_FLAG != 0 {
                        writeln!(line, "{rel:>11.6} {ch}  ErrorFrame")?;
                    } else if *fd {
                        let dlc = len_to_dlc(*len);
                        let brs = (*fd_flags & CANFD_BRS != 0) as u8;
                        let esi = (*fd_flags & CANFD_ESI != 0) as u8;
                        let mut flags = 1u32 << 12;
                        if brs != 0 {
                            flags |= 1 << 13;
                        }
                        if esi != 0 {
                            flags |= 1 << 14;
                        }
                        let dir = if *fd_flags & CAN_TX_LOCAL != 0 { "Tx" } else { "Rx" };
                        write!(line, "{rel:>11.6} CANFD {ch:>3} {dir}   {idtxt:>9}                                   {brs} {esi} {dlc:x} {:>2}", data.len())?;
                        for b in data.iter() {
                            write!(line, " {b:02X}")?;
                        }
                        writeln!(line, "        0    0 {flags:>8X}        0        0        0        0        0")?;
                    } else if id & CAN_RTR_FLAG != 0 {
                        let dir = if *fd_flags & CAN_TX_LOCAL != 0 { "Tx" } else { "Rx" };
                        writeln!(line, "{rel:>11.6} {ch}  {idtxt:<15} {dir}   r {len:x}")?;
                    } else {
                        let dir = if *fd_flags & CAN_TX_LOCAL != 0 { "Tx" } else { "Rx" };
                        write!(line, "{rel:>11.6} {ch}  {idtxt:<15} {dir}   d {len:x}")?;
                        for b in data.iter() {
                            write!(line, " {b:02X}")?;
                        }
                        line.push(b'\n');
                    }
                    frames += 1;
                }
                (Record::Mark(t), Format::Asc) => {
                    let first = *asc_first_ts.get_or_insert(from_ts.unwrap_or(ctx.start_ts).min(ts));
                    let rel = ts.saturating_sub(first) as f64 / 1e9;
                    writeln!(line, "// {rel:.6} mark: {t}")?;
                }
                _ => {}
            }
            buf.extend_from_slice(&line);
            if buf.len() >= 1 << 16 {
                out.write_all(&buf)?;
                buf.clear();
            }
        }
        Ok(true)
    })?;

    if fmt == Format::Asc {
        buf.extend_from_slice(b"End TriggerBlock\n");
    }
    if !complete {
        let note = match fmt {
            Format::Asc => "// export truncated: data was overwritten while reading\n",
            Format::Candump => "",
        };
        buf.extend_from_slice(note.as_bytes());
    }
    out.write_all(&buf)?;
    out.flush()?;
    Ok(frames)
}

pub fn len_to_dlc(len: u8) -> u8 {
    match len {
        0..=8 => len,
        9..=12 => 9,
        13..=16 => 10,
        17..=20 => 11,
        21..=24 => 12,
        25..=32 => 13,
        33..=48 => 14,
        _ => 15,
    }
}

/// Human readable dump of every record of a session (debugging aid).
pub fn dump_records<S: BlockStore, W: Write>(ring: &mut Ring<S>, s: &SessionInfo, out: &mut W) -> io::Result<()> {
    ring.for_each_block(s.first_seq, s.last_seq, |h, blk| {
        writeln!(out, "# block seq={} session={} flags={:#x} records={} used={} name={:?}", h.seq, h.session, h.flags, h.record_count, h.used, h.name)?;
        for (ts, r) in RecordIter::new(blk, h) {
            match r {
                Record::Can { .. } => {}
                other => writeln!(out, "{:.6} {:?}", ts as f64 / 1e9, other)?,
            }
        }
        Ok(true)
    })?;
    Ok(())
}
