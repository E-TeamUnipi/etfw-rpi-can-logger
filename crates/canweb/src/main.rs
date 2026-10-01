//! canweb: the page you open on a phone/laptop joined to the logger's
//! Wi-Fi hotspot. Status, live data, session naming, time sync, downloads.
//!
//! Reads the ring partition directly (read-only) for exports, and talks to
//! canlogd over its control socket for everything else. If this process
//! crashes, recording is not affected.
//!
//! The analysis app (webapp/, served from GitHub Pages over HTTPS) uses the
//! same API from the browser: CORS allows the origins in `app_origin`, and
//! Chrome's Local Network Access lets an HTTPS page call this HTTP address.

use axum::body::Body;
use axum::extract::{Path as AxPath, Query, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use canlog_core::config::{Config, DEFAULT_CTL_SOCKET, DEFAULT_PATH};
use canlog_core::export::{self, Format, Options};
use canlog_core::ring::{Ring, RingDevice, SessionInfo};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

const UI: &str = include_str!("ui.html");

struct App {
    ctl: PathBuf,
    ring: PathBuf,
    ui_file: Option<PathBuf>,
    pretend_online: bool,
    device_name: String,
    /// Browser origins allowed to call the API (CORS).
    origins: Vec<String>,
    /// Required for sending frames; sending is refused without one.
    pin: Option<String>,
    /// Where the analysis app is published (linked from the status page).
    app_url: String,
}

type St = State<Arc<App>>;

// ---------------------------------------------------------------- control socket

async fn ctl(app: &App, cmd: &Value) -> Result<String, String> {
    let fut = async {
        let mut s = UnixStream::connect(&app.ctl).await.map_err(|e| format!("logger not running ({e})"))?;
        let mut line = cmd.to_string();
        line.push('\n');
        s.write_all(line.as_bytes()).await.map_err(|e| e.to_string())?;
        let mut r = BufReader::new(s);
        let mut resp = String::new();
        r.read_line(&mut resp).await.map_err(|e| e.to_string())?;
        Ok::<_, String>(resp)
    };
    tokio::time::timeout(Duration::from_secs(3), fut).await.map_err(|_| "logger timeout".to_string())?
}

fn json_text(s: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")], s).into_response()
}

fn error(code: StatusCode, msg: impl ToString) -> Response {
    (code, Json(json!({"ok": false, "error": msg.to_string()}))).into_response()
}

async fn api_status(State(app): St) -> Response {
    match ctl(&app, &json!({"cmd": "status"})).await {
        Ok(s) => json_text(s),
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, e),
    }
}

async fn api_live(State(app): St) -> Response {
    match ctl(&app, &json!({"cmd": "live"})).await {
        Ok(s) => json_text(s),
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, e),
    }
}

async fn api_cmd(State(app): St, Json(mut body): Json<Value>) -> Response {
    let cmd = body.get("cmd").and_then(|c| c.as_str()).unwrap_or("").to_string();
    if !["timesync", "name", "mark", "new_session", "sim_powerfail", "send", "tx_mode"].contains(&cmd.as_str()) {
        return error(StatusCode::BAD_REQUEST, "unknown command");
    }
    if cmd == "send" || cmd == "tx_mode" {
        match &app.pin {
            None => return error(StatusCode::FORBIDDEN, "sending is disabled: set control_pin in logger.conf"),
            Some(p) if body.get("pin").and_then(|v| v.as_str()) != Some(p.as_str()) => {
                return error(StatusCode::FORBIDDEN, "wrong PIN");
            }
            _ => {}
        }
    }
    if let Some(o) = body.as_object_mut() {
        o.remove("pin");
    }
    if cmd == "timesync" && body.get("source").is_none() {
        body["source"] = json!("http");
    }
    match ctl(&app, &body).await {
        Ok(s) => json_text(s),
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, e),
    }
}

/// Every received frame, as binary records (canlog_core::proto::STREAM_RECORD),
/// until the client goes away. Interface indices match `ifaces[].idx` in
/// /api/status.
async fn api_stream(State(app): St) -> Response {
    let open = async {
        let mut s = UnixStream::connect(&app.ctl).await.map_err(|e| format!("logger not running ({e})"))?;
        s.write_all(b"{\"cmd\":\"stream\"}\n").await.map_err(|e| e.to_string())?;
        let mut r = BufReader::new(s);
        let mut line = String::new();
        r.read_line(&mut line).await.map_err(|e| e.to_string())?;
        if !line.contains("\"ok\":true") {
            return Err(line);
        }
        // binary data that arrived together with the answer line
        let first = r.buffer().to_vec();
        Ok::<_, String>((first, r.into_inner()))
    };
    let (first, mut sock) = match tokio::time::timeout(Duration::from_secs(3), open).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return error(StatusCode::SERVICE_UNAVAILABLE, e),
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "logger timeout"),
    };
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(16);
    tokio::spawn(async move {
        if !first.is_empty() && tx.send(Ok(Bytes::from(first))).await.is_err() {
            return;
        }
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match sock.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(Ok(Bytes::copy_from_slice(&buf[..n]))).await.is_err() {
                        break; // browser went away; dropping the socket ends the stream
                    }
                }
            }
        }
    });
    let mut resp = Response::new(Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)));
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

// ---------------------------------------------------------------- CORS

async fn cors(State(app): St, req: Request, next: Next) -> Response {
    let origin = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()).map(str::to_string);
    let allowed = origin.filter(|o| app.origins.iter().any(|a| a == o || a == "*"));
    let preflight = req.method() == Method::OPTIONS;
    let mut resp = if preflight { StatusCode::NO_CONTENT.into_response() } else { next.run(req).await };
    let h = resp.headers_mut();
    h.append(header::VARY, HeaderValue::from_static("Origin"));
    if let Some(o) = allowed.and_then(|o| HeaderValue::from_str(&o).ok()) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, o);
        if preflight {
            h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST"));
            h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("content-type"));
            h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
            // Private Network Access preflights (older Chrome versions)
            h.insert("access-control-allow-private-network", HeaderValue::from_static("true"));
        }
    }
    resp
}

// ---------------------------------------------------------------- sessions

fn open_ring(path: &PathBuf) -> io::Result<Ring<RingDevice>> {
    let dev = RingDevice::open(path, false)?;
    if dev.sb.is_none() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "ring not initialised yet"));
    }
    Ok(Ring::new(dev))
}

fn list_sessions(path: &PathBuf) -> io::Result<Vec<SessionInfo>> {
    Ok(open_ring(path)?.sessions())
}

async fn api_sessions(State(app): St) -> Response {
    let path = app.ring.clone();
    match tokio::task::spawn_blocking(move || list_sessions(&path)).await {
        Ok(Ok(mut v)) => {
            v.reverse(); // newest first
            let out: Vec<Value> = v
                .iter()
                .map(|s| {
                    let mut j = serde_json::to_value(s).unwrap();
                    j["file_candump"] = json!(export::file_name(s, Format::Candump));
                    j["file_asc"] = json!(export::file_name(s, Format::Asc));
                    j["size_kb"] = json!(s.blocks * 4);
                    j
                })
                .collect();
            Json(out).into_response()
        }
        Ok(Err(e)) => error(StatusCode::SERVICE_UNAVAILABLE, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct DlQuery {
    fmt: Option<String>,
    gz: Option<u8>,
    from: Option<f64>,
    to: Option<f64>,
    last_s: Option<f64>,
    iface: Option<String>,
}

/// `io::Write` that forwards 64 KiB chunks into an HTTP body stream.
struct ChanWriter {
    tx: mpsc::Sender<Result<Bytes, io::Error>>,
    buf: Vec<u8>,
}

impl Write for ChanWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(b);
        if self.buf.len() >= 1 << 16 {
            self.flush()?;
        }
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = Bytes::from(std::mem::replace(&mut self.buf, Vec::with_capacity(1 << 16)));
        self.tx.blocking_send(Ok(chunk)).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "client went away"))
    }
}

async fn api_download(State(app): St, AxPath(id): AxPath<u32>, Query(q): Query<DlQuery>) -> Response {
    let fmt = Format::parse(q.fmt.as_deref().unwrap_or("candump")).unwrap_or(Format::Candump);
    let gz = q.gz.unwrap_or(0) != 0;
    let path = app.ring.clone();

    // find the session first so errors become proper HTTP errors
    let p2 = path.clone();
    let session = match tokio::task::spawn_blocking(move || list_sessions(&p2)).await {
        Ok(Ok(v)) => v.into_iter().find(|s| s.id == id),
        Ok(Err(e)) => return error(StatusCode::SERVICE_UNAVAILABLE, e),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let Some(s) = session else { return error(StatusCode::NOT_FOUND, "no such session") };

    let mut opt = Options { from_s: q.from, to_s: q.to, ifaces: Vec::new() };
    if let Some(last) = q.last_s {
        opt.from_s = Some((s.duration_s - last).max(0.0));
    }
    if let Some(i) = q.iface.filter(|s| !s.is_empty()) {
        opt.ifaces = i.split(',').map(|x| x.to_string()).collect();
    }
    let mut fname = export::file_name(&s, fmt);
    if gz {
        fname.push_str(".gz");
    }

    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(8);
    tokio::task::spawn_blocking(move || {
        let run = || -> io::Result<()> {
            let mut ring = open_ring(&path)?;
            let w = ChanWriter { tx: tx.clone(), buf: Vec::with_capacity(1 << 16) };
            if gz {
                let mut g = flate2::write::GzEncoder::new(w, flate2::Compression::new(3));
                export::export(&mut ring, &s, fmt, &opt, &mut g)?;
                g.finish()?.flush()?;
            } else {
                let mut w = w;
                export::export(&mut ring, &s, fmt, &opt, &mut w)?;
                w.flush()?;
            }
            Ok(())
        };
        if let Err(e) = run() {
            if e.kind() != io::ErrorKind::BrokenPipe {
                eprintln!("canweb: export failed: {e}");
                let _ = tx.blocking_send(Err(e));
            }
        }
    });

    let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    let mime = if gz { "application/gzip" } else { "text/plain; charset=utf-8" };
    let mut resp = Response::new(body);
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{fname}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

// ---------------------------------------------------------------- page

async fn index(State(app): St) -> Response {
    let page = match &app.ui_file {
        Some(p) => std::fs::read_to_string(p).unwrap_or_else(|_| UI.to_string()),
        None => UI.to_string(),
    };
    let page = page.replace("{{DEVICE_NAME}}", &app.device_name).replace("{{APP_URL}}", &app.app_url);
    ([(header::CACHE_CONTROL, "no-store")], Html(page)).into_response()
}

// Connectivity checks. Answering "online" keeps phones from dropping the
// hotspot or routing our traffic over mobile data.
async fn generate_204(State(app): St) -> Response {
    if app.pretend_online {
        StatusCode::NO_CONTENT.into_response()
    } else {
        index(State(app)).await
    }
}
async fn apple_check(State(app): St) -> Response {
    if app.pretend_online {
        Html("<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>").into_response()
    } else {
        index(State(app)).await
    }
}
async fn ms_connecttest() -> &'static str {
    "Microsoft Connect Test"
}
async fn ms_ncsi() -> &'static str {
    "Microsoft NCSI"
}

fn main() {
    let mut cfg_path = PathBuf::from(DEFAULT_PATH);
    let mut ctl_path = PathBuf::from(DEFAULT_CTL_SOCKET);
    let mut ring: Option<PathBuf> = None;
    let mut listen = None;
    let mut ui_file = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-c" => cfg_path = it.next().expect("value").into(),
            "-s" => ctl_path = it.next().expect("value").into(),
            "-r" => ring = Some(it.next().expect("value").into()),
            "-l" => listen = Some(it.next().expect("value")),
            "--ui" => ui_file = Some(PathBuf::from(it.next().expect("value"))),
            _ => {
                eprintln!("usage: canweb [-c logger.conf] [-s control.sock] [-r ring-device] [-l addr:port] [--ui ui.html]");
                std::process::exit(2);
            }
        }
    }
    let cfg = Config::load(&cfg_path);
    let listen = listen.unwrap_or_else(|| cfg.str_or("http_listen", "0.0.0.0:80"));
    let app = Arc::new(App {
        ctl: ctl_path,
        ring: ring.unwrap_or_else(|| cfg.ring_dev().into()),
        ui_file,
        pretend_online: cfg.bool_or("http_pretend_online", true),
        device_name: cfg.str_or("device_name", "CAN logger"),
        origins: cfg
            .str_or("app_origin", "https://e-teamunipi.github.io")
            .split(',')
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        pin: cfg.get("control_pin").or(cfg.get("ble_pin")).map(str::to_string),
        app_url: cfg.str_or("app_url", "https://e-teamunipi.github.io/etfw-rpi-can-logger/"),
    });

    let router = Router::new()
        .route("/", get(index))
        .route("/api/status", get(api_status))
        .route("/api/live", get(api_live))
        .route("/api/cmd", post(api_cmd))
        .route("/api/stream", get(api_stream))
        .route("/api/sessions", get(api_sessions))
        .route("/api/sessions/{id}/download", get(api_download))
        .route("/generate_204", get(generate_204))
        .route("/gen_204", get(generate_204))
        .route("/hotspot-detect.html", get(apple_check))
        .route("/library/test/success.html", get(apple_check))
        .route("/connecttest.txt", get(ms_connecttest))
        .route("/ncsi.txt", get(ms_ncsi))
        .fallback(get(index))
        .layer(middleware::from_fn_with_state(app.clone(), cors))
        .with_state(app);

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async move {
        let listener = loop {
            match tokio::net::TcpListener::bind(&listen).await {
                Ok(l) => break l,
                Err(e) => {
                    eprintln!("canweb: bind {listen}: {e}, retrying");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        };
        eprintln!("canweb: listening on http://{listen}");
        axum::serve(listener, router).await.unwrap();
    });
}
