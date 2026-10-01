//! Control socket protocol between canlogd and its clients (web, BLE, CLI).
//!
//! Newline-delimited JSON over a Unix stream socket. Each request line gets
//! one response line, except `subscribe`, which streams a status line every
//! second until the client disconnects, and `stream`, which answers one line
//! and then switches the connection to binary frame records (see
//! `STREAM_RECORD`).

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Current status (no live table).
    Status,
    /// Live table: last frame per CAN id with rates.
    Live {
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Stream status once per second.
    Subscribe,
    /// Wall-clock time from a phone/browser.
    Timesync {
        utc_ms: i64,
        #[serde(default)]
        source: Option<String>,
    },
    /// Name the current session.
    Name { name: String },
    /// Put a marker in the log ("brake test starts now").
    Mark { text: String },
    /// End the current session and start a new one without rebooting.
    NewSession {
        #[serde(default)]
        name: Option<String>,
    },
    /// Simulator only: pretend the power-fail input changed.
    SimPowerfail { on: bool },
    /// Every received frame as binary records, until the client disconnects.
    /// A slow client loses batches rather than slowing the logger.
    Stream,
    /// Send one frame. Only on interfaces with `can.<x>.tx = 1`. The first
    /// send switches the interface out of listen-only (it then ACKs frames)
    /// with one-shot transmission, and it stays that way until `tx_mode`
    /// turns it off or the logger restarts.
    Send {
        /// Interface name (can0) or label.
        iface: String,
        id: u32,
        #[serde(default)]
        ext: bool,
        #[serde(default)]
        fd: bool,
        #[serde(default)]
        brs: bool,
        /// Payload as hex.
        data: String,
    },
    /// Switch an interface between normal (TX) mode and listen-only.
    TxMode { iface: String, on: bool },
}

/// Binary stream record, little endian: `ts_ns i64` (Unix time when the
/// logger clock is synced, else boot time), `id u32` (without flag bits),
/// `iface u8`, `flags u8` (STREAM_F_*), `len u8`, `0 u8`, `data[len]`.
pub const STREAM_RECORD: usize = 16;
pub const STREAM_F_EXT: u8 = 1;
pub const STREAM_F_RTR: u8 = 2;
pub const STREAM_F_ERR: u8 = 4;
pub const STREAM_F_FD: u8 = 8;
pub const STREAM_F_BRS: u8 = 16;
pub const STREAM_F_ESI: u8 = 32;
pub const STREAM_F_TX: u8 = 64;

/// Hex payload ("DEADBEEF", spaces allowed) to bytes.
pub fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

pub fn ok() -> serde_json::Value {
    serde_json::json!({"ok": true})
}

pub fn err(msg: impl std::fmt::Display) -> serde_json::Value {
    serde_json::json!({"ok": false, "error": msg.to_string()})
}

/// Blocking one-shot request (used by the CLI and tests).
pub fn request(sock: &Path, cmd: &Command) -> io::Result<serde_json::Value> {
    let mut s = UnixStream::connect(sock)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = serde_json::to_vec(cmd)?;
    line.push(b'\n');
    s.write_all(&line)?;
    let mut r = BufReader::new(s);
    let mut resp = String::new();
    r.read_line(&mut resp)?;
    serde_json::from_str(&resp).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
