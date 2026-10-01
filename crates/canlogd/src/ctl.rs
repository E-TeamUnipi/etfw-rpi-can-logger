//! Control socket server (newline-delimited JSON, see canlog_core::proto).

use crate::{Request, Shared};
use canlog_core::proto::{err, ok, Command};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::Duration;

pub fn spawn(path: &Path, shared: Arc<Shared>, tx: Sender<Request>) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let listener = UnixListener::bind(path)?;
    std::thread::Builder::new().name("ctl".into()).spawn(move || {
        for conn in listener.incoming().flatten() {
            let (s, t) = (shared.clone(), tx.clone());
            let _ = std::thread::Builder::new().name("ctl-client".into()).spawn(move || client(conn, s, t));
        }
    })?;
    Ok(())
}

/// Binary frame stream: batches from the receive loop, written as they come.
fn stream(mut w: UnixStream, shared: Arc<Shared>) {
    let (tx, rx) = mpsc::sync_channel::<Arc<Vec<u8>>>(256);
    shared.streams.lock().unwrap().push(tx);
    shared.stream_count.fetch_add(1, Relaxed);
    let _ = w.set_write_timeout(Some(Duration::from_secs(10)));
    if w.write_all(format!("{}\n", ok()).as_bytes()).is_ok() {
        while let Ok(batch) = rx.recv() {
            if w.write_all(&batch).is_err() {
                break;
            }
        }
    }
    // the receive loop drops our sender on its next failed send
    shared.stream_count.fetch_sub(1, Relaxed);
}

fn client(conn: UnixStream, shared: Arc<Shared>, tx: Sender<Request>) {
    let _ = conn.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(mut w) = conn.try_clone() else { return };
    let r = BufReader::new(conn);
    for line in r.lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Command>(&line) {
            Err(e) => err(format!("bad command: {e}")).to_string(),
            Ok(Command::Status) => shared.status_json.lock().unwrap().clone(),
            Ok(Command::Live { limit }) => {
                let live = shared.live.lock().unwrap();
                let n = limit.unwrap_or(usize::MAX).min(live.len());
                serde_json::to_string(&live[..n]).unwrap_or_else(|_| "[]".into())
            }
            Ok(Command::Subscribe) => {
                loop {
                    let s = shared.status_json.lock().unwrap().clone();
                    if w.write_all(s.as_bytes()).and_then(|_| w.write_all(b"\n")).is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            Ok(Command::Stream) => return stream(w, shared),
            Ok(Command::SimPowerfail { .. }) if !shared.sim => err("only available with --sim").to_string(),
            Ok(Command::Name { name }) if name.len() > 200 => err("name too long").to_string(),
            Ok(cmd @ (Command::Send { .. } | Command::TxMode { .. })) => {
                // these answer with the result from the receive loop
                let (rtx, rrx) = mpsc::channel();
                if tx.send(Request { cmd, reply: Some(rtx) }).is_err() {
                    return;
                }
                shared.wake();
                match rrx.recv_timeout(Duration::from_secs(5)) {
                    Ok(v) => v.to_string(),
                    Err(_) => err("logger did not answer").to_string(),
                }
            }
            Ok(cmd) => {
                if tx.send(Request { cmd, reply: None }).is_err() {
                    return;
                }
                shared.wake();
                ok().to_string()
            }
        };
        if w.write_all(resp.as_bytes()).and_then(|_| w.write_all(b"\n")).is_err() {
            return;
        }
    }
}
