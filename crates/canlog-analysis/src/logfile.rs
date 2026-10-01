//! Incremental readers for candump `.log` and Vector `.asc` files. Feed the
//! (already decompressed) bytes in chunks of any size.

use crate::dataset::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fmt {
    Candump,
    Asc,
}

pub struct LogReader {
    fmt: Option<Fmt>,
    partial: Vec<u8>,
    // ASC state
    hex: bool,
    relative: bool,
    asc_base_ns: Option<i64>,
    asc_last_ns: i64,
    asc_channels: Vec<(u32, String)>,
    pub lines: u64,
    pub bad_lines: u64,
    pub first_error: Option<String>,
}

impl Default for LogReader {
    fn default() -> Self {
        LogReader {
            fmt: None,
            partial: Vec::new(),
            hex: true,
            relative: false,
            asc_base_ns: None,
            asc_last_ns: 0,
            asc_channels: Vec::new(),
            lines: 0,
            bad_lines: 0,
            first_error: None,
        }
    }
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn parse_hex(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 8 {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

fn hex_bytes(s: &str, out: &mut Vec<u8>) -> Option<()> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    for p in b.chunks(2) {
        out.push(hexval(p[0])? << 4 | hexval(p[1])?);
    }
    Some(())
}

/// "1700000000.123456" -> ns, without float rounding.
fn parse_secs(s: &str) -> Option<i64> {
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (a, b) = s.split_once('.').unwrap_or((s, ""));
    let sec: i64 = if a.is_empty() { 0 } else { a.parse().ok()? };
    let mut frac: i64 = 0;
    for (i, c) in b.bytes().take(9).enumerate() {
        if !c.is_ascii_digit() {
            return None;
        }
        frac += (c - b'0') as i64 * 10i64.pow(8 - i as u32);
    }
    let v = sec.checked_mul(1_000_000_000)? + frac;
    Some(if neg { -v } else { v })
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `date Mon Jan 01 10:00:00.000 am 2024` (taken as UTC) -> Unix ns.
fn parse_asc_date(rest: &str) -> Option<i64> {
    let t: Vec<&str> = rest.split_whitespace().collect();
    let mi = t.iter().position(|w| MONTHS.iter().any(|m| m.eq_ignore_ascii_case(w)))?;
    let month = MONTHS.iter().position(|m| m.eq_ignore_ascii_case(t[mi]))? as i64 + 1;
    let day: i64 = t.get(mi + 1)?.parse().ok()?;
    let time = t.get(mi + 2)?;
    let mut hms = time.split(':');
    let mut h: i64 = hms.next()?.parse().ok()?;
    let m: i64 = hms.next()?.parse().ok()?;
    let sec_ns = parse_secs(hms.next().unwrap_or("0"))?;
    let mut year_idx = mi + 3;
    if let Some(ap) = t.get(mi + 3) {
        let ap = ap.to_ascii_lowercase();
        if ap == "am" || ap == "pm" {
            if ap == "pm" && h < 12 {
                h += 12;
            }
            if ap == "am" && h == 12 {
                h = 0;
            }
            year_idx += 1;
        }
    }
    let year: i64 = t.get(year_idx)?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    Some((days * 86400 + h * 3600 + m * 60) * 1_000_000_000 + sec_ns)
}

impl LogReader {
    pub fn push(&mut self, chunk: &[u8], ds: &mut Dataset) {
        let mut start = 0;
        for (i, &c) in chunk.iter().enumerate() {
            if c == b'\n' {
                if self.partial.is_empty() {
                    self.line(&chunk[start..i], ds);
                } else {
                    self.partial.extend_from_slice(&chunk[start..i]);
                    let l = std::mem::take(&mut self.partial);
                    self.line(&l, ds);
                }
                start = i + 1;
            }
        }
        self.partial.extend_from_slice(&chunk[start..]);
    }

    pub fn finish(&mut self, ds: &mut Dataset) {
        if !self.partial.is_empty() {
            let l = std::mem::take(&mut self.partial);
            self.line(&l, ds);
        }
    }

    pub fn format_name(&self) -> &'static str {
        match self.fmt {
            Some(Fmt::Candump) => "candump",
            Some(Fmt::Asc) => "asc",
            None => "unknown",
        }
    }

    fn bad(&mut self, line: &str, why: &str) {
        self.bad_lines += 1;
        if self.first_error.is_none() {
            self.first_error = Some(format!("line {}: {why}: {}", self.lines, line.chars().take(80).collect::<String>()));
        }
    }

    fn line(&mut self, raw: &[u8], ds: &mut Dataset) {
        self.lines += 1;
        let s = String::from_utf8_lossy(raw);
        let line = s.trim();
        if line.is_empty() {
            return;
        }
        if self.fmt.is_none() {
            self.fmt = if line.starts_with('(') {
                Some(Fmt::Candump)
            } else if line.starts_with("date") || line.starts_with("base") || line.starts_with("//") || line.starts_with("Begin") {
                Some(Fmt::Asc)
            } else {
                return self.bad(line, "unknown log format (expected candump -l or Vector ASC)");
            };
        }
        match self.fmt {
            Some(Fmt::Candump) => {
                if self.candump(line, ds).is_none() {
                    self.bad(line, "not a candump frame");
                }
            }
            Some(Fmt::Asc) => {
                if self.asc(line, ds).is_none() {
                    self.bad(line, "unreadable ASC line");
                }
            }
            None => {}
        }
    }

    fn candump(&mut self, line: &str, ds: &mut Dataset) -> Option<()> {
        let mut it = line.split_whitespace();
        let ts = parse_secs(it.next()?.strip_prefix('(')?.strip_suffix(')')?)?;
        if ds.is_empty() {
            // candump -l writes Unix time; anything after 2001 is wall clock
            ds.wall_clock = ts > 1_000_000_000_000_000_000;
        }
        let bus = ds.bus_index(it.next()?);
        let frame = it.next()?;
        let dir_tx = it.next() == Some("T");
        let (idtxt, rest) = frame.split_once('#')?;
        let raw = parse_hex(idtxt)?;
        let mut flags = if dir_tx { F_TX } else { 0 };
        let id;
        if idtxt.len() == 8 {
            if raw & 0x2000_0000 != 0 {
                flags |= F_ERR;
                id = raw & 0x1FFF_FFFF;
            } else {
                flags |= F_EXT;
                id = raw & 0x1FFF_FFFF;
            }
        } else {
            id = raw & 0x7FF;
        }
        let mut data = Vec::with_capacity(8);
        if let Some(fd) = rest.strip_prefix('#') {
            if fd.starts_with('#') {
                return Some(()); // CAN XL: not supported, skip quietly
            }
            let mut b = fd.bytes();
            let fl = hexval(b.next()?)?;
            flags |= F_FD;
            if fl & 1 != 0 {
                flags |= F_BRS;
            }
            if fl & 2 != 0 {
                flags |= F_ESI;
            }
            hex_bytes(&fd[1..].replace('.', ""), &mut data)?;
        } else if let Some(r) = rest.strip_prefix('R').or_else(|| rest.strip_prefix('r')) {
            flags |= F_RTR;
            let n = r.bytes().next().and_then(hexval).unwrap_or(0).min(8);
            data.resize(n as usize, 0);
        } else {
            hex_bytes(&rest.replace('.', ""), &mut data)?;
        }
        ds.push(ts, bus, id, flags, &data);
        Some(())
    }

    fn asc_bus(&mut self, ds: &mut Dataset, ch: u32) -> u16 {
        match self.asc_channels.iter().find(|c| c.0 == ch) {
            Some((_, n)) => ds.bus_index(&n.clone()),
            None => ds.bus_index(&format!("CH{ch}")),
        }
    }

    fn asc_time(&mut self, t: &str) -> Option<i64> {
        let v = parse_secs(t)?;
        let v = if self.relative { self.asc_last_ns + v } else { v };
        self.asc_last_ns = v;
        Some(v + self.asc_base_ns.unwrap_or(0))
    }

    fn asc_id(&self, s: &str) -> Option<(u32, bool)> {
        let (t, ext) = match s.strip_suffix(['x', 'X']) {
            Some(t) => (t, true),
            None => (s, false),
        };
        let v = if self.hex { parse_hex(t)? } else { t.parse().ok()? };
        Some((v, ext || v > 0x7FF))
    }

    fn asc(&mut self, line: &str, ds: &mut Dataset) -> Option<()> {
        if let Some(c) = line.strip_prefix("//") {
            let c = c.trim();
            // our own exporter: "// channel 1 = can0", "// 12.5 mark: text"
            if let Some(r) = c.strip_prefix("channel ") {
                if let Some((n, name)) = r.split_once('=') {
                    if let Ok(n) = n.trim().parse::<u32>() {
                        self.asc_channels.push((n, name.trim().to_string()));
                    }
                }
            } else if let Some((t, text)) = c.split_once(" mark: ") {
                if let Some(ns) = parse_secs(t.trim()) {
                    ds.markers.push((ns + self.asc_base_ns.unwrap_or(0), text.to_string()));
                }
            }
            return Some(());
        }
        let t: Vec<&str> = line.split_whitespace().collect();
        match t[0] {
            "date" => {
                if let Some(ns) = parse_asc_date(&line[4..]) {
                    self.asc_base_ns = Some(ns);
                    ds.wall_clock = true;
                }
                return Some(());
            }
            "base" => {
                self.hex = t.get(1) != Some(&"dec");
                self.relative = line.contains("relative");
                return Some(());
            }
            "Begin" | "End" | "internal" | "no" => return Some(()),
            _ => {}
        }
        if t.len() < 3 || parse_secs(t[0]).is_none() {
            return Some(()); // other ASC event types
        }
        if t[1] == "CANFD" {
            // ts CANFD ch dir id [name] brs esi dlc len data...
            let ts = self.asc_time(t[0])?;
            let ch: u32 = t.get(2)?.parse().ok()?;
            let dir_tx = t.get(3).is_some_and(|d| d.eq_ignore_ascii_case("tx"));
            if t.get(4).is_some_and(|w| w.eq_ignore_ascii_case("ErrorFrame")) {
                let bus = self.asc_bus(ds, ch);
                ds.push(ts, bus, 0, F_ERR, &[]);
                return Some(());
            }
            let (id, ext) = self.asc_id(t.get(4)?)?;
            let mut k = 5;
            // optional symbolic name: anything that is not a single digit
            if t.get(k).is_some_and(|w| !(w.len() == 1 && w.as_bytes()[0].is_ascii_digit())) {
                k += 1;
            }
            let brs = *t.get(k)? == "1";
            let esi = *t.get(k + 1)? == "1";
            let len: usize = t.get(k + 3)?.parse().ok()?;
            let mut data = Vec::with_capacity(len);
            for w in t.get(k + 4..k + 4 + len)? {
                data.push(u8::from_str_radix(w, 16).ok()?);
            }
            let mut flags = F_FD;
            if ext {
                flags |= F_EXT;
            }
            if brs {
                flags |= F_BRS;
            }
            if esi {
                flags |= F_ESI;
            }
            if dir_tx {
                flags |= F_TX;
            }
            let bus = self.asc_bus(ds, ch);
            ds.push(ts, bus, id, flags, &data);
            return Some(());
        }
        let Ok(ch) = t[1].parse::<u32>() else {
            return Some(()); // statistics, triggers, ...
        };
        if t[2].eq_ignore_ascii_case("ErrorFrame") {
            let ts = self.asc_time(t[0])?;
            let bus = self.asc_bus(ds, ch);
            ds.push(ts, bus, 0, F_ERR, &[]);
            return Some(());
        }
        // ts ch id dir d|r dlc data...
        if t.len() < 5 {
            return Some(());
        }
        let ts = self.asc_time(t[0])?;
        let (id, ext) = self.asc_id(t[2])?;
        let dir_tx = t[3].eq_ignore_ascii_case("tx");
        let mut flags = if ext { F_EXT } else { 0 };
        if dir_tx {
            flags |= F_TX;
        }
        let dlc = t.get(5).and_then(|d| u8::from_str_radix(d, 16).ok()).unwrap_or(0).min(8) as usize;
        let mut data = Vec::with_capacity(dlc);
        match t[4] {
            "r" | "R" => {
                flags |= F_RTR;
                data.resize(dlc, 0);
            }
            "d" | "D" => {
                for w in t.get(6..6 + dlc)? {
                    data.push(u8::from_str_radix(w, 16).ok()?);
                }
            }
            _ => return Some(()),
        }
        let bus = self.asc_bus(ds, ch);
        ds.push(ts, bus, id, flags, &data);
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str, chunk: usize) -> (Dataset, LogReader) {
        let mut ds = Dataset::default();
        let mut r = LogReader::default();
        for c in text.as_bytes().chunks(chunk) {
            r.push(c, &mut ds);
        }
        r.finish(&mut ds);
        (ds, r)
    }

    #[test]
    fn candump() {
        let text = "(1700000000.000100) can0 123#DEADBEEF\n(1700000000.000200) can1 18FEF1FE#0102\n\
                    (1700000000.000300) can0 20000080#0000000000000000\n(1700000000.000400) can0 200##1AABBCCDDEEFF0011\n\
                    (1700000000.000500) can0 7DF#R8 T\n";
        for chunk in [1, 7, 4096] {
            let (ds, r) = read(text, chunk);
            assert_eq!(r.bad_lines, 0, "{:?}", r.first_error);
            assert_eq!(ds.len(), 5);
            assert!(ds.wall_clock);
            assert_eq!(ds.bus_names, ["can0", "can1"]);
            let f = ds.frame(0);
            assert_eq!((f.ts_ns, f.id, f.data), (1_700_000_000_000_100_000, 0x123, &[0xDE, 0xAD, 0xBE, 0xEF][..]));
            assert!(ds.frame(1).ext() && ds.frame(1).id == 0x18FEF1FE);
            assert_eq!(ds.frame(2).flags & F_ERR, F_ERR);
            let fd = ds.frame(3);
            assert_eq!(fd.flags & (F_FD | F_BRS), F_FD | F_BRS);
            assert_eq!(fd.data.len(), 8);
            let rtr = ds.frame(4);
            assert_eq!(rtr.flags & (F_RTR | F_TX), F_RTR | F_TX);
            assert_eq!(rtr.data.len(), 8);
            assert_eq!(ds.frames_of(0, 0x123), &[0]);
        }
    }

    #[test]
    fn asc() {
        let text = "date Mon Jan 01 01:00:00.000 pm 2024\nbase hex  timestamps absolute\ninternal events logged\n\
                    // channel 1 = can0\n// channel 2 = chassis\nBegin Triggerblock Mon Jan 01 01:00:00.000 pm 2024\n   0.000000 Start of measurement\n\
                       0.001000 1  123             Rx   d 2 11 22\n   0.002000 2  18FEF1FEx       Tx   d 1 FF\n\
                       0.003000 CANFD   1 Rx         200                                   1 0 9 12 00 01 02 03 04 05 06 07 08 09 0A 0B        0    0     3000        0        0        0        0        0\n\
                       0.003500 CANFD   2 Rx   201  SomeMsg  0 0 2 2 AA BB\n\
                       0.004000 1  ErrorFrame\n   0.005000 1  7DF             Rx   r 8\n// 0.004500 mark: brake test\nEnd TriggerBlock\n";
        let (ds, r) = read(text, 5);
        assert_eq!(r.bad_lines, 0, "{:?}", r.first_error);
        assert_eq!(ds.len(), 6);
        assert!(ds.wall_clock);
        assert_eq!(ds.bus_names, ["can0", "chassis"]);
        let base = 1_704_114_000_000_000_000i64; // 2024-01-01 13:00:00 UTC
        assert_eq!(ds.frame(0).ts_ns, base + 1_000_000);
        assert_eq!(ds.frame(0).data, &[0x11, 0x22]);
        assert!(ds.frame(1).ext() && ds.frame(1).flags & F_TX != 0);
        assert_eq!(ds.frame(2).data.len(), 12);
        assert_eq!(ds.frame(3).data, &[0xAA, 0xBB]);
        assert_eq!(ds.frame(3).bus, 1);
        assert_eq!(ds.frame(4).flags, F_ERR);
        assert_eq!(ds.frame(5).flags & F_RTR, F_RTR);
        assert_eq!(ds.markers, vec![(base + 4_500_000, "brake test".to_string())]);
    }
}
