//! canble: Bluetooth LE GATT service for the CAN logger.
//!
//! Meant for quick checks from a phone (Web Bluetooth page, see webapp/):
//! status and live data at ~1 Hz, time sync, naming, markers, Wi-Fi on/off.
//! Bulk downloads go over Wi-Fi.
//!
//! Service 6e7c0001-4f3a-4b8e-9a6d-2c1b0e5f7a10
//!   6e7c0002  status   read    compact JSON
//!   6e7c0003  live     read    compact JSON, most recent frames per id
//!   6e7c0004  command  write   JSON line(s) terminated by '\n'
//!
//! Values can be longer than the MTU; clients use long reads (Web Bluetooth
//! does this automatically). A read at offset 0 takes a fresh snapshot, reads
//! at higher offsets continue the same snapshot.

use bluer::adv::{Advertisement, Type as AdvType};
use bluer::gatt::local::{
    Application, Characteristic, CharacteristicRead, CharacteristicWrite, CharacteristicWriteMethod, ReqError, Service,
};
use bluer::{Address, Uuid};
use canlog_core::config::{Config, DEFAULT_CTL_SOCKET, DEFAULT_PATH};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const SERVICE: Uuid = Uuid::from_u128(0x6e7c0001_4f3a_4b8e_9a6d_2c1b0e5f7a10);
const CH_STATUS: Uuid = Uuid::from_u128(0x6e7c0002_4f3a_4b8e_9a6d_2c1b0e5f7a10);
const CH_LIVE: Uuid = Uuid::from_u128(0x6e7c0003_4f3a_4b8e_9a6d_2c1b0e5f7a10);
const CH_CMD: Uuid = Uuid::from_u128(0x6e7c0004_4f3a_4b8e_9a6d_2c1b0e5f7a10);

const WIFI_OFF_FLAG: &str = "/run/wifi.disabled";

struct Ctx {
    ctl: PathBuf,
    pin: Option<String>,
    ssid: String,
    ap_ip: String,
    /// snapshot per (device, characteristic) for long reads
    snapshots: Mutex<HashMap<(Address, u8), Vec<u8>>>,
    /// partial command lines per device
    pending: Mutex<HashMap<Address, Vec<u8>>>,
    last_result: Mutex<String>,
}

async fn ctl(path: &Path, cmd: &Value) -> Result<Value, String> {
    let fut = async {
        let mut s = UnixStream::connect(path).await.map_err(|e| format!("logger not running: {e}"))?;
        s.write_all(format!("{cmd}\n").as_bytes()).await.map_err(|e| e.to_string())?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).await.map_err(|e| e.to_string())?;
        serde_json::from_str(&line).map_err(|e| e.to_string())
    };
    tokio::time::timeout(Duration::from_secs(3), fut).await.map_err(|_| "timeout".to_string())?
}

fn wifi_enabled() -> bool {
    !Path::new(WIFI_OFF_FLAG).exists()
}

fn wifi_up() -> bool {
    std::fs::read_to_string("/sys/class/net/wlan0/operstate").map(|s| s.trim() == "up").unwrap_or(false)
}

/// The hotspot script writes the SSID it actually used (it may add a MAC suffix).
fn current_ssid(ctx: &Ctx) -> String {
    std::fs::read_to_string("/run/wifi.ssid").map(|s| s.trim().to_string()).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| ctx.ssid.clone())
}

fn set_wifi(on: bool) {
    if on {
        let _ = std::fs::remove_file(WIFI_OFF_FLAG);
    } else {
        let _ = std::fs::write(WIFI_OFF_FLAG, b"ble\n");
        let _ = std::process::Command::new("killall").arg("hostapd").status();
    }
}

/// Compact status for BLE (full status is ~1-2 KB, this is ~300-600 bytes).
async fn status_bytes(ctx: &Ctx) -> Vec<u8> {
    let s = match ctl(&ctx.ctl, &json!({"cmd": "status"})).await {
        Ok(v) => v,
        Err(e) => return json!({"st": "offline", "err": e}).to_string().into_bytes(),
    };
    let ifs: Vec<Value> = s["ifaces"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|i| {
                    json!({"n": i["name"], "l": i["label"], "br": i["bitrate"], "st": i["state"], "p": i["present"],
                           "fps": i["fps"], "ld": i["load_pct"], "lp": i["load_peak_pct"], "err": i["errors"], "lo": i["listen_only"],
                           "ta": i["tx_allowed"], "tx": i["tx_mode"], "dbr": i["dbitrate"]})
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "st": s["state"], "sim": s["sim"],
        "sid": s["session"]["id"], "name": s["session"]["name"], "dur": s["session"]["duration_s"],
        "fps": s["rec"]["fps"], "fr": s["rec"]["frames"], "drop": s["rec"]["dropped_frames"],
        "werr": s["rec"]["write_errors"], "lost": s["rec"]["lost_blocks"],
        "t": s["time"]["utc_ms"], "tsrc": s["time"]["source"],
        "pf": s["power"]["fail"], "pfn": s["power"]["events"],
        "ring": s["ring"]["used_pct"], "wrap": s["ring"]["wrapped"], "hrs": s["ring"]["hours_at_current_rate"],
        "up": s["uptime_s"],
        "wifi": {"on": wifi_enabled(), "up": wifi_up(), "ssid": current_ssid(ctx), "ip": ctx.ap_ip},
        "res": *ctx.last_result.lock().unwrap(),
        "if": ifs,
    })
    .to_string()
    .into_bytes()
}

/// Most recently seen ids first, up to 40 rows: [bus, id, data, hz, age_ms]
async fn live_bytes(ctx: &Ctx) -> Vec<u8> {
    let v = match ctl(&ctx.ctl, &json!({"cmd": "live"})).await {
        Ok(v) => v,
        Err(_) => return b"[]".to_vec(),
    };
    let mut rows: Vec<&Value> = v.as_array().map(|a| a.iter().collect()).unwrap_or_default();
    rows.sort_by_key(|r| (r["age_ms"].as_u64().unwrap_or(u64::MAX) > 2000, r["if"].as_str().map(str::to_string), r["id"].as_str().map(str::to_string)));
    let out: Vec<Value> = rows
        .into_iter()
        .take(40)
        .map(|r| {
            let data = r["data"].as_str().unwrap_or("");
            let data = if data.len() > 32 { &data[..32] } else { data };
            json!([r["if"], r["id"], data, r["hz"], r["age_ms"]])
        })
        .collect();
    Value::Array(out).to_string().into_bytes()
}

async fn handle_line(ctx: &Ctx, line: &str) -> String {
    let mut v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return format!("bad json: {e}"),
    };
    if let Some(pin) = &ctx.pin {
        if v.get("pin").and_then(|p| p.as_str()) != Some(pin.as_str()) {
            return "wrong pin".into();
        }
    }
    if let Some(o) = v.as_object_mut() {
        o.remove("pin");
    }
    let cmd = v["cmd"].as_str().unwrap_or("").to_string();
    match cmd.as_str() {
        "wifi" => {
            let on = v["on"].as_bool().unwrap_or(true);
            set_wifi(on);
            format!("wifi {}", if on { "on" } else { "off" })
        }
        "send" | "tx_mode" if ctx.pin.is_none() => format!("{cmd} refused: set control_pin in logger.conf to allow sending"),
        "timesync" | "name" | "mark" | "new_session" | "send" | "tx_mode" => {
            if cmd == "timesync" {
                v["source"] = json!("ble");
            }
            match ctl(&ctx.ctl, &v).await {
                Ok(r) if r["ok"].as_bool() == Some(true) => format!("{cmd} ok"),
                Ok(r) => format!("{cmd} failed: {}", r["error"]),
                Err(e) => format!("{cmd} failed: {e}"),
            }
        }
        _ => format!("unknown command {cmd:?}"),
    }
}

fn read_char(uuid: Uuid, which: u8, ctx: Arc<Ctx>) -> Characteristic {
    Characteristic {
        uuid,
        read: Some(CharacteristicRead {
            read: true,
            fun: Box::new(move |req| {
                let ctx = ctx.clone();
                Box::pin(async move {
                    let key = (req.device_address, which);
                    let off = req.offset as usize;
                    let val = if off == 0 {
                        let v = if which == 0 { status_bytes(&ctx).await } else { live_bytes(&ctx).await };
                        ctx.snapshots.lock().unwrap().insert(key, v.clone());
                        v
                    } else {
                        ctx.snapshots.lock().unwrap().get(&key).cloned().unwrap_or_default()
                    };
                    if off > val.len() {
                        return Err(ReqError::InvalidOffset);
                    }
                    Ok(val[off..].to_vec())
                })
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn cmd_char(ctx: Arc<Ctx>) -> Characteristic {
    Characteristic {
        uuid: CH_CMD,
        write: Some(CharacteristicWrite {
            write: true,
            write_without_response: true,
            method: CharacteristicWriteMethod::Fun(Box::new(move |value, req| {
                let ctx = ctx.clone();
                Box::pin(async move {
                    let lines: Vec<String> = {
                        let mut p = ctx.pending.lock().unwrap();
                        let buf = p.entry(req.device_address).or_default();
                        if req.offset == 0 && !buf.is_empty() && !buf.contains(&b'\n') && value.first() == Some(&b'{') {
                            buf.clear(); // stale fragment from an aborted write
                        }
                        buf.extend_from_slice(&value);
                        if buf.len() > 4096 {
                            buf.clear();
                            return Err(ReqError::InvalidValueLength);
                        }
                        let mut lines = Vec::new();
                        while let Some(i) = buf.iter().position(|&b| b == b'\n') {
                            let l: Vec<u8> = buf.drain(..=i).collect();
                            lines.push(String::from_utf8_lossy(&l).trim().to_string());
                        }
                        lines
                    };
                    for l in lines.into_iter().filter(|l| !l.is_empty()) {
                        let r = handle_line(&ctx, &l).await;
                        eprintln!("canble: {} -> {r}", req.device_address);
                        *ctx.last_result.lock().unwrap() = r;
                    }
                    Ok(())
                })
            })),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn serve(ctx: Arc<Ctx>, name: &str) -> bluer::Result<()> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;
    let addr = adapter.address().await?;
    let name = if name.is_empty() { format!("CANLog-{:02X}{:02X}", addr.0[4], addr.0[5]) } else { name.to_string() };
    adapter.set_alias(name.clone()).await?;
    eprintln!("canble: advertising as {name} on {} ({addr})", adapter.name());

    let adv = Advertisement {
        advertisement_type: AdvType::Peripheral,
        service_uuids: [SERVICE].into_iter().collect(),
        local_name: Some(name.clone()),
        discoverable: Some(true),
        ..Default::default()
    };
    let _adv = adapter.advertise(adv).await?;
    let app = Application {
        services: vec![Service {
            uuid: SERVICE,
            primary: true,
            characteristics: vec![read_char(CH_STATUS, 0, ctx.clone()), read_char(CH_LIVE, 1, ctx.clone()), cmd_char(ctx.clone())],
            ..Default::default()
        }],
        ..Default::default()
    };
    let _app = adapter.serve_gatt_application(app).await?;

    // keep running while the adapter is healthy; forget per-device state now and then
    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;
        if !adapter.is_powered().await.unwrap_or(false) {
            return Err(bluer::Error { kind: bluer::ErrorKind::NotReady, message: "adapter powered off".into() });
        }
        let mut s = ctx.snapshots.lock().unwrap();
        if s.len() > 32 {
            s.clear();
        }
    }
}

fn main() {
    let mut cfg_path = PathBuf::from(DEFAULT_PATH);
    let mut ctl_path = PathBuf::from(DEFAULT_CTL_SOCKET);
    let mut dump: Option<Option<String>> = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-c" => cfg_path = it.next().expect("value").into(),
            "-s" => ctl_path = it.next().expect("value").into(),
            // print what the BLE characteristics would return (and optionally run a command), then exit
            "--dump" => dump = Some(it.next()),
            _ => {
                eprintln!("usage: canble [-c logger.conf] [-s control.sock] [--dump [JSON-COMMAND]]");
                std::process::exit(2);
            }
        }
    }
    let cfg = Config::load(&cfg_path);
    if dump.is_none() && !cfg.bool_or("ble", true) {
        eprintln!("canble: disabled in config");
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    let ctx = Arc::new(Ctx {
        ctl: ctl_path,
        // control_pin also guards sending over Wi-Fi; ble_pin is the older name
        pin: cfg.get("control_pin").or(cfg.get("ble_pin")).map(str::to_string),
        ssid: cfg.str_or("wifi_ssid", "ET-18"),
        ap_ip: cfg.str_or("wifi_ip", "192.168.4.1"),
        snapshots: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        last_result: Mutex::new(String::new()),
    });
    let name = cfg.str_or("ble_name", "");
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    if let Some(cmd) = dump {
        rt.block_on(async move {
            if let Some(c) = cmd {
                println!("command: {}", handle_line(&ctx, &c).await);
            }
            let s = status_bytes(&ctx).await;
            println!("status ({} bytes): {}", s.len(), String::from_utf8_lossy(&s));
            let l = live_bytes(&ctx).await;
            println!("live ({} bytes): {}", l.len(), String::from_utf8_lossy(&l));
        });
        return;
    }
    rt.block_on(async move {
        loop {
            if let Err(e) = serve(ctx.clone(), &name).await {
                eprintln!("canble: {e}; retrying in 3 s");
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });
}
