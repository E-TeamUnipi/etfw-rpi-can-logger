//! JavaScript interface of the analysis engine. Results are JSON strings
//! (parsed with `JSON.parse` on the JS side); bulk data uses typed arrays.
//! The web app runs this in a Web Worker (webapp/js/worker.js).

use canlog_analysis::engine::{Engine as Core, RtaOverride};
use canlog_analysis::rta::RtaOptions;
use serde::Serialize;
use std::collections::HashMap;
use wasm_bindgen::prelude::*;

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("{{\"error\":{:?}}}", e.to_string()))
}

fn parse<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, JsError> {
    serde_json::from_str(s).map_err(|e| JsError::new(&format!("bad JSON argument: {e}")))
}

#[wasm_bindgen]
pub struct Engine {
    core: Core,
}

#[wasm_bindgen]
impl Engine {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Engine {
        Engine { core: Core::new() }
    }

    /// Maximum frames kept in memory for live data (older half dropped).
    pub fn set_live_cap(&mut self, frames: usize) {
        self.core.live_cap = frames;
    }

    // ---- DBCs

    pub fn load_dbc(&mut self, key: &str, bytes: &[u8]) -> String {
        json(&self.core.load_dbc(key, bytes))
    }

    pub fn remove_dbc(&mut self, key: &str) {
        self.core.remove_dbc(key)
    }

    /// `keys`: JSON array of DBC keys for this bus.
    pub fn assign(&mut self, bus: &str, keys: &str) -> Result<(), JsError> {
        self.core.assign(bus, parse(keys)?);
        Ok(())
    }

    pub fn messages(&self, bus: &str) -> String {
        json(&self.core.messages(bus))
    }

    /// `values`: JSON object {signal: physical value}. Returns the payload.
    pub fn encode(&self, bus: &str, msg: &str, values: &str) -> Result<Vec<u8>, JsError> {
        let map: HashMap<String, f64> = parse(values)?;
        let v: Vec<(String, f64)> = map.into_iter().collect();
        self.core.encode(bus, msg, &v).ok_or_else(|| JsError::new(&format!("no message {msg} on {bus}")))
    }

    // ---- data in

    pub fn clear(&mut self) {
        self.core.clear()
    }

    pub fn import_begin(&mut self) {
        self.core.import_begin()
    }

    pub fn import_push(&mut self, chunk: &[u8]) {
        self.core.import_push(chunk)
    }

    pub fn import_end(&mut self) -> String {
        json(&self.core.import_end())
    }

    /// `buses`: JSON array, logger interface index -> bus name.
    pub fn live_begin(&mut self, buses: &str) -> Result<(), JsError> {
        let b: Vec<String> = parse(buses)?;
        self.core.live_begin(&b);
        Ok(())
    }

    pub fn live_set_buses(&mut self, buses: &str) -> Result<(), JsError> {
        let b: Vec<String> = parse(buses)?;
        self.core.live_set_buses(&b);
        Ok(())
    }

    /// Returns how many bytes were used; keep the rest for the next call.
    pub fn live_push(&mut self, buf: &[u8]) -> usize {
        self.core.live_push(buf)
    }

    pub fn push_frame(&mut self, bus: &str, ts_ms: f64, id: u32, flags: u8, data: &[u8]) {
        self.core.push_frame(bus, (ts_ms * 1e6) as i64, id, flags, data)
    }

    pub fn summary(&self) -> String {
        json(&self.core.summary("", 0, 0, None))
    }

    // ---- views

    /// `bitrates`: JSON object {bus: [nominal, data]}.
    pub fn bus_load(&self, bitrates: &str) -> Result<String, JsError> {
        let m: HashMap<String, (u32, u32)> = parse(bitrates)?;
        Ok(json(&self.core.bus_load(&m)))
    }

    pub fn msg_stats(&self, bus: &str, bitrate: u32, dbitrate: u32) -> String {
        #[derive(Serialize)]
        struct Row<'a> {
            #[serde(flatten)]
            s: &'a canlog_analysis::stats::MsgStats,
            name: &'a Option<String>,
        }
        let v = self.core.msg_stats(bus, bitrate, dbitrate);
        let rows: Vec<Row> = v.iter().map(|(s, name)| Row { s, name }).collect();
        json(&rows)
    }

    pub fn latest(&self, bus: &str) -> String {
        json(&self.core.latest(bus))
    }

    /// Interleaved [t, v, ...] for one signal between t0 and t1.
    pub fn series(&mut self, bus: &str, msg: &str, sig: &str, t0: f64, t1: f64, px: usize) -> Vec<f64> {
        self.core.series(bus, msg, sig, t0, t1, px)
    }

    /// `opts`: {bitrate, dbitrate, sim_seconds, sim_runs};
    /// `overrides`: [{id, ext, period_ms, jitter_ms, deadline_ms}].
    pub fn rta(&self, bus: &str, opts: &str, overrides: &str) -> Result<String, JsError> {
        let o: RtaOptions = parse(opts)?;
        let ov: Vec<RtaOverride> = parse(overrides)?;
        Ok(json(&self.core.rta(bus, &o, &ov)))
    }
}

impl Default for Engine {
    fn default() -> Self {
        Engine::new()
    }
}
