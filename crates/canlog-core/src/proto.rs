//! Control socket protocol between canlogd and its clients (web, BLE, CLI).
//!
//! Newline-delimited JSON over a Unix stream socket. Each request line gets
//! one response line, except `subscribe`, which streams a status line every
//! second until the client disconnects.

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
