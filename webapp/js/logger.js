// Connection to the logger, over Bluetooth (GATT, canble) or Wi-Fi (HTTP
// API of canweb at http://192.168.4.1, reachable from this HTTPS page via
// Chrome's Local Network Access). Both look the same to the views:
// `logger.status` (normalised), events 'status' and 'state', `command()`.
// Live frames go to the engine: the full stream over Wi-Fi, the ~1 Hz
// snapshot of the last frame per id over Bluetooth.

import * as engine from './engine.js';
import { parseHex } from './ui.js';

const SVC = '6e7c0001-4f3a-4b8e-9a6d-2c1b0e5f7a10';
const CH_STATUS = '6e7c0002-4f3a-4b8e-9a6d-2c1b0e5f7a10';
const CH_LIVE = '6e7c0003-4f3a-4b8e-9a6d-2c1b0e5f7a10';
const CH_CMD = '6e7c0004-4f3a-4b8e-9a6d-2c1b0e5f7a10';

const dec = new TextDecoder(), enc = new TextEncoder();

export const logger = new EventTarget();
Object.assign(logger, {
  kind: null,        // 'ble' | 'wifi' | null
  state: 'idle',     // idle | connecting | connected | lost
  status: null,
  error: null,
  addr: '192.168.4.1',
  pin: '',
  live: false,       // frames are feeding the engine
  liveFrames: 0,
});

const emit = (type, detail) => logger.dispatchEvent(new CustomEvent(type, { detail }));
function setState(s, err = null) {
  logger.state = s;
  logger.error = err;
  emit('state');
}

export const bleSupported = () => 'bluetooth' in navigator;

/** Bus name used in datasets and profiles: the adapter label, else its name. */
export const busName = i => i.label || i.name;

// ------------------------------------------------------------------ status

function fromWifi(s) {
  return {
    state: s.state, sim: s.sim,
    session: { id: s.session?.id, name: s.session?.name ?? '', duration_s: s.session?.duration_s, marks: s.session?.marks },
    fps: s.rec?.fps, frames: s.rec?.frames, dropped: s.rec?.dropped_frames,
    write_errors: s.rec?.write_errors, lost: s.rec?.lost_blocks,
    time: { utc_ms: s.time?.utc_ms, source: s.time?.source },
    ring: { used_pct: s.ring?.used_pct, wrapped: s.ring?.wrapped, hours: s.ring?.hours_at_current_rate, size_mb: s.ring?.size_mb },
    power_fail: s.power?.fail, uptime_s: s.uptime_s, wifi: null, result: null,
    ifaces: (s.ifaces ?? []).map(i => ({
      idx: i.idx, name: i.name, label: i.label, bitrate: i.bitrate, dbitrate: i.dbitrate, present: i.present, state: i.state,
      fps: i.fps, load: i.load_pct, peak: i.load_peak_pct, max: i.load_max_pct, errors: i.errors, listen_only: i.listen_only,
      tx_allowed: i.tx_allowed, tx_mode: i.tx_mode, serial: i.serial, usb_port: i.usb_port, config_error: i.config_error,
    })),
  };
}

function fromBle(s) {
  return {
    state: s.st, sim: s.sim,
    session: { id: s.sid, name: s.name ?? '', duration_s: s.dur },
    fps: s.fps, frames: s.fr, dropped: s.drop, write_errors: s.werr, lost: s.lost,
    time: { utc_ms: s.t, source: s.tsrc },
    ring: { used_pct: s.ring, wrapped: s.wrap, hours: s.hrs },
    power_fail: s.pf, uptime_s: s.up, wifi: s.wifi, result: s.res,
    ifaces: (s.if ?? []).map((i, idx) => ({
      idx, name: i.n, label: i.l, bitrate: i.br, dbitrate: i.dbr, present: i.p, state: i.st, fps: i.fps, load: i.ld,
      peak: i.lp, errors: i.err, listen_only: i.lo, tx_allowed: i.ta, tx_mode: i.tx,
    })),
  };
}

function setStatus(st) {
  const prev = logger.status;
  logger.status = st;
  emit('status');
  const names = st.ifaces.map(busName).join('\n');
  if (logger.live && prev && prev.ifaces.map(busName).join('\n') !== names && logger.kind === 'wifi') {
    engine.call('live_set_buses', JSON.stringify(st.ifaces.map(busName)));
  }
}

// ------------------------------------------------------------------ Wi-Fi

const base = () => `http://${logger.addr}`;

// Chrome's Local Network Access: private IP literals (192.168.4.1) are known
// to be local; a host name (e.g. "et18" via the hotspot's DNS) needs the hint.
// Loopback (development) needs none.
export const lna = () => /^(\d+\.\d+\.\d+\.\d+|localhost|\[[0-9a-f:]+\])(:\d+)?$/i.test(logger.addr) ? {} : { targetAddressSpace: 'local' };

async function http(path, opts = {}) {
  const r = await fetch(base() + path, { ...lna(), cache: 'no-store', ...opts });
  const body = await r.json().catch(() => ({}));
  if (!r.ok || body.ok === false) throw new Error(body.error || `HTTP ${r.status}`);
  return body;
}

let pollTimer = null, streamAbort = null;

async function pollWifi() {
  try {
    setStatus(fromWifi(await http('/api/status')));
    if (logger.state !== 'connected') setState('connected');
  } catch (e) {
    if (logger.state === 'connected') setState('lost', e.message);
  }
}

export async function connectWifi(addr) {
  await disconnect();
  logger.addr = addr || logger.addr;
  logger.kind = 'wifi';
  setState('connecting');
  try {
    setStatus(fromWifi(await http('/api/status')));
  } catch (e) {
    logger.kind = null;
    setState('idle', `Cannot reach ${logger.addr}: ${e.message}. Join the logger's Wi-Fi, and allow "local network access" when the browser asks.`);
    throw e;
  }
  setState('connected');
  await command({ cmd: 'timesync', utc_ms: Date.now() }).catch(() => {});
  pollTimer = setInterval(pollWifi, 1000);
}

/** Start feeding the full frame stream into the engine (Wi-Fi only). */
export async function startStream() {
  if (logger.kind !== 'wifi') return startBleLive();
  stopStream();
  await engine.call('live_begin', JSON.stringify(logger.status.ifaces.map(busName)));
  logger.live = true;
  logger.liveFrames = 0;
  emit('live');
  streamAbort = new AbortController();
  const signal = streamAbort.signal;
  (async () => {
    try {
      const r = await fetch(base() + '/api/stream', { ...lna(), cache: 'no-store', signal });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const reader = r.body.getReader();
      let rest = new Uint8Array(0);
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        let buf = value;
        if (rest.length) {
          buf = new Uint8Array(rest.length + value.length);
          buf.set(rest);
          buf.set(value, rest.length);
        }
        const used = await engine.call('live_push', buf);
        rest = buf.slice(used);
        logger.liveFrames += used / 20;
      }
    } catch (e) {
      if (e.name !== 'AbortError') console.warn('stream', e);
    }
    if (!signal.aborted) {
      logger.live = false;
      emit('live');
    }
  })();
}

export function stopStream() {
  streamAbort?.abort();
  streamAbort = null;
  stopBleLive();
  if (logger.live) {
    logger.live = false;
    emit('live');
  }
}

export async function sessions() {
  const r = await fetch(base() + '/api/sessions', { ...lna(), cache: 'no-store' });
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return r.json();
}

export const downloadUrl = (id, q) => `${base()}/api/sessions/${id}/download?${new URLSearchParams(q)}`;

/** Download a session as a file. Fetched by script (a plain link from this
 *  HTTPS page to the logger's HTTP address would be blocked as insecure). */
export async function saveSession(s, fmt) {
  const r = await fetch(downloadUrl(s.id, { fmt, gz: 1 }), lna());
  if (!r.ok) throw new Error(`download failed: HTTP ${r.status}`);
  return r.blob();
}

/** Fetch a session from the logger and import it into the engine. */
export async function importSession(s, progress) {
  // ASC rather than candump: it also carries markers and which frames we sent
  const r = await fetch(downloadUrl(s.id, { fmt: 'asc', gz: 1 }), lna());
  if (!r.ok) throw new Error(`download failed: HTTP ${r.status}`);
  stopStream();
  return engine.callProgress('importStream', progress, r.body, s.file_asc + '.gz');
}

// ------------------------------------------------------------------ Bluetooth

let dev = null, chStatus = null, chLive = null, chCmd = null, bleTimer = null, bleLive = false;
let gattChain = Promise.resolve();
const gatt = fn => (gattChain = gattChain.then(fn, fn));
const lastSeen = new Map();

async function readJson(ch) {
  return JSON.parse(dec.decode(await gatt(() => ch.readValue())));
}

async function pollBle() {
  if (!dev?.gatt.connected) return;
  try {
    setStatus(fromBle(await readJson(chStatus)));
    if (bleLive) feedBleLive(await readJson(chLive));
  } catch (e) {
    console.warn('ble poll', e);
  }
}

/** BLE live rows [iface, idHex, dataHex, hz, ageMs] become frames when they change. */
function feedBleLive(rows) {
  const now = Date.now();
  const byName = new Map((logger.status?.ifaces ?? []).map(i => [i.name, busName(i)]));
  for (const [iface, idHex, dataHex, , age] of rows) {
    const key = iface + ':' + idHex + ':' + dataHex;
    const ts = now - (age ?? 0);
    const prev = lastSeen.get(iface + ':' + idHex);
    if (prev && prev.key === key && Math.abs(prev.ts - ts) < 1500) continue;
    lastSeen.set(iface + ':' + idHex, { key, ts });
    const ext = idHex.length > 3;
    const data = parseHex(dataHex) ?? new Uint8Array(0);
    engine.call('push_frame', byName.get(iface) ?? iface, ts, parseInt(idHex, 16), (ext ? 1 : 0) | (data.length > 8 ? 8 : 0), data);
    logger.liveFrames++;
  }
}

async function startBleLive() {
  await engine.call('live_begin', '[]');
  lastSeen.clear();
  bleLive = true;
  logger.live = true;
  logger.liveFrames = 0;
  emit('live');
}

function stopBleLive() {
  bleLive = false;
}

async function connectGatt() {
  setState('connecting');
  const server = await dev.gatt.connect();
  const svc = await server.getPrimaryService(SVC);
  [chStatus, chLive, chCmd] = await Promise.all([CH_STATUS, CH_LIVE, CH_CMD].map(u => svc.getCharacteristic(u)));
  gattChain = Promise.resolve();
  logger.kind = 'ble';
  await command({ cmd: 'timesync', utc_ms: Date.now() });
  setStatus(fromBle(await readJson(chStatus)));
  setState('connected');
  bleTimer = setInterval(pollBle, 1000);
}

export async function connectBle() {
  await disconnect();
  if (!dev) {
    dev = await navigator.bluetooth.requestDevice({ filters: [{ services: [SVC] }] });
    dev.addEventListener('gattserverdisconnected', () => {
      clearInterval(bleTimer);
      if (logger.kind === 'ble') setState('lost', 'Bluetooth connection lost');
    });
  }
  try {
    await connectGatt();
  } catch (e) {
    setState('idle', e.message);
    throw e;
  }
}

export const bleName = () => dev?.name;

// ------------------------------------------------------------------ common

/** Send a command ({cmd, ...}). Over Bluetooth the result arrives in status.result. */
export async function command(obj) {
  const body = { ...obj };
  if (logger.pin) body.pin = logger.pin;
  if (logger.kind === 'wifi') {
    return http('/api/cmd', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  }
  if (logger.kind === 'ble') {
    const bytes = enc.encode(JSON.stringify(body) + '\n');
    await gatt(() => chCmd.writeValueWithResponse ? chCmd.writeValueWithResponse(bytes) : chCmd.writeValue(bytes));
    return { ok: true, pending: true };
  }
  throw new Error('not connected');
}

export async function disconnect() {
  stopStream();
  clearInterval(pollTimer);
  clearInterval(bleTimer);
  pollTimer = bleTimer = null;
  if (logger.kind === 'ble' && dev?.gatt.connected) dev.gatt.disconnect();
  logger.kind = null;
  logger.status = null;
  setState('idle');
  emit('status');
}
