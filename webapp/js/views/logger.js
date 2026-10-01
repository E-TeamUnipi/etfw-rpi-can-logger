// Logger tab: connect (Wi-Fi or Bluetooth), status, session controls,
// recorded sessions (download, or open in the app).

import { logger, connectWifi, connectBle, disconnect, command, sessions, saveSession, bleSupported, busName, bleName } from '../logger.js';
import { dataset, openSession, startLive } from '../dataset.js';
import * as store from '../store.js';
import { h, fill, toast, ask, fmtDur, fmtNum, download } from '../ui.js';

let el, statusSec, busesSec, sessSec, connSec;
let nameDirty = false;

export function init(root) {
  el = root;
  connSec = h('section');
  statusSec = h('section', { hidden: true });
  busesSec = h('section', { hidden: true });
  sessSec = h('section', { hidden: true });
  el.append(connSec, statusSec, busesSec, sessSec);
  logger.addEventListener('state', renderConn);
  logger.addEventListener('status', renderStatus);
  logger.addEventListener('live', renderConn);
  store.setting('addr', '192.168.4.1').then(a => { logger.addr = a; renderConn(); });
  store.setting('pin', '').then(p => { logger.pin = p; });
}

export function show() {
  if (logger.kind === 'wifi') loadSessions();
}

function renderConn() {
  const connected = logger.state === 'connected';
  const addr = h('input', { type: 'text', value: logger.addr, 'aria-label': 'Logger address', autocomplete: 'off', size: 14 });
  const pin = h('input', { type: 'password', value: logger.pin, placeholder: 'PIN (if set)', 'aria-label': 'PIN', autocomplete: 'off', inputmode: 'numeric', size: 8 });
  pin.onchange = () => { logger.pin = pin.value.trim(); store.setSetting('pin', logger.pin); };
  const wifi = async () => {
    try {
      await connectWifi(addr.value.trim());
      await store.setSetting('addr', logger.addr);
      if (!dataset.kind || dataset.kind === 'live') await startLive();
      loadSessions();
    } catch (e) { toast(e.message, 6000); }
  };
  const ble = async () => {
    try {
      await connectBle();
      if (!dataset.kind || dataset.kind === 'live') await startLive();
    } catch (e) { if (e.name !== 'NotFoundError') toast(e.message, 6000); }
  };
  fill(connSec,
    h('h2', {}, h('span', {}, 'Connection'),
      connected && h('button', { value: 'secondary', onclick: disconnect }, 'Disconnect')),
    connected
      ? h('p', {}, `Connected over ${logger.kind === 'ble' ? `Bluetooth (${bleName() ?? 'logger'})` : `Wi-Fi to ${logger.addr}`}. `,
          logger.live ? h('span', {}, 'Live data is feeding the Data, Analysis and Plots tabs.')
            : h('button', { value: 'secondary', onclick: () => startLive().catch(e => toast(e.message)) }, 'Use live data'))
      : [
        h('p', {}, h('b', {}, 'Wi-Fi: '), 'join the logger\'s Wi-Fi (ET-18), then connect. Chrome asks once to allow access to devices on your local network: allow it. Gives every frame live, session downloads and sending.'),
        h('form', { onsubmit: e => { e.preventDefault(); wifi(); } },
          addr, pin, h('button', {}, logger.state === 'connecting' && logger.kind === 'wifi' ? 'Connecting…' : 'Connect over Wi-Fi')),
        h('p', {}, h('b', {}, 'Bluetooth: '), 'status, ~1 Hz live values, commands. ',
          bleSupported() ? '' : h('small', {}, 'This browser has no Web Bluetooth (on iPhone use the Bluefy browser).')),
        h('form', { onsubmit: e => { e.preventDefault(); ble(); } },
          h('button', { disabled: !bleSupported() }, 'Connect over Bluetooth')),
        logger.error && h('p', {}, h('mark', { 'data-state': 'bad' }, 'error'), ' ', logger.error),
        h('p', {}, h('small', {}, 'Without a logger: open a log file in the Data tab. Everything works offline once the app is installed.')),
      ]);
  if (!connected) {
    statusSec.hidden = busesSec.hidden = sessSec.hidden = true;
  }
}

let figs, clock, wifiForm, nameIn, titleEl;

function buildStatus() {
  const run = async (obj, ok) => {
    try {
      await command(obj);
      toast(ok);
    } catch (e) { toast(e.message, 5000); }
  };
  nameIn = h('input', { type: 'text', placeholder: 'Name this recording', maxlength: 60, autocomplete: 'off' });
  nameIn.oninput = () => (nameDirty = true);
  const markIn = h('input', { type: 'text', placeholder: 'Marker text', maxlength: 100, autocomplete: 'off' });
  figs = h('dl');
  clock = h('p');
  wifiForm = h('form', { hidden: true });
  titleEl = h('span');
  fill(statusSec,
    h('h2', {}, titleEl),
    figs,
    h('form', { onsubmit: e => { e.preventDefault(); nameDirty = false; run({ cmd: 'name', name: nameIn.value }, 'Name saved'); } },
      nameIn, h('button', {}, 'Save name')),
    h('form', { onsubmit: e => { e.preventDefault(); run({ cmd: 'mark', text: markIn.value || 'marker' }, 'Marker added'); markIn.value = ''; } },
      markIn, h('button', { value: 'secondary' }, 'Add marker'),
      h('button', { type: 'button', value: 'secondary', onclick: async () => {
        if (await ask('Close this session and start a new one?', 'New session')) { nameDirty = false; run({ cmd: 'new_session' }, 'New session started'); }
      } }, 'New session')),
    clock, wifiForm);
  wifiForm.append(h('label', {}, h('input', { type: 'checkbox', role: 'switch', onchange: e => run({ cmd: 'wifi', on: e.target.checked }, 'Wi-Fi ' + (e.target.checked ? 'on' : 'off')) }), h('span')));
}

function renderStatus() {
  const s = logger.status;
  if (!s) return;
  statusSec.hidden = busesSec.hidden = false;
  if (!figs) buildStatus();
  titleEl.textContent = `Session ${s.session.id != null ? '#' + s.session.id : ''}`;
  if (!nameDirty && document.activeElement !== nameIn) nameIn.value = s.session.name;
  fill(figs,
    h('div', {}, h('dt', {}, 'duration'), h('dd', {}, fmtDur(s.session.duration_s))),
    h('div', {}, h('dt', {}, 'frames/s'), h('dd', {}, s.fps == null ? '–' : Math.round(s.fps))),
    h('div', {}, h('dt', {}, 'frames'), h('dd', {}, fmtNum(s.frames))),
    h('div', {}, h('dt', {}, 'dropped'), h('dd', {}, fmtNum(s.dropped))),
    h('div', {}, h('dt', {}, 'ring used'), h('dd', {}, s.ring.wrapped ? 'full (wraps)' : (s.ring.used_pct ?? '–') + '%')),
    h('div', {}, h('dt', {}, 'holds at this rate'), h('dd', {}, s.ring.hours == null ? '–' : s.ring.hours > 48 ? (s.ring.hours / 24).toFixed(1) + ' d' : s.ring.hours + ' h')),
    h('div', {}, h('dt', {}, 'write errors / lost'), h('dd', {}, `${s.write_errors ?? 0} / ${s.lost ?? 0}`)),
    h('div', {}, h('dt', {}, 'supply'), h('dd', { style: s.power_fail ? 'color:var(--bad)' : '' }, s.power_fail ? 'DOWN' : 'ok')));
  fill(clock, h('small', {}, s.time.utc_ms ? `Logger clock ${new Date(s.time.utc_ms).toLocaleString()} (from ${s.time.source}).` : 'Logger clock not set.',
    s.result ? ` Last command: ${s.result}.` : ''));
  wifiForm.hidden = !s.wifi;
  if (s.wifi) {
    const sw = wifiForm.querySelector('input');
    if (document.activeElement !== sw) sw.checked = s.wifi.on;
    wifiForm.querySelector('label span').textContent = `Wi-Fi hotspot ${s.wifi.ssid} (${s.wifi.on ? (s.wifi.up ? 'on' : 'starting') : 'off'})`;
  }

  fill(busesSec,
    h('h2', {}, h('span', {}, 'Buses')),
    s.ifaces.length ? h('figure', {}, h('table', {},
      h('thead', {}, h('tr', {}, ['Bus', 'Interface', 'State', 'kbit/s', 'frames/s', 'load', 'peak 100 ms', 'session max', 'errors', 'mode'].map((t, i) => h('th', i > 2 ? { 'data-num': '' } : {}, t)))),
      h('tbody', {}, s.ifaces.map(i => {
        const st = !i.present ? ['bad', 'unplugged'] : i.state === 'bus-off' ? ['bad', 'bus-off'] : i.state === 'error-passive' ? ['warn', 'error passive'] : i.fps > 0 ? ['ok', 'receiving'] : ['', 'idle'];
        return h('tr', {},
          h('td', {}, busName(i)), h('td', {}, i.name), h('td', {}, h('mark', { 'data-state': st[0] }, st[1])),
          h('td', { 'data-num': '' }, i.bitrate ? i.bitrate / 1000 + (i.dbitrate ? '/' + i.dbitrate / 1000 : '') : 'virtual'),
          h('td', { 'data-num': '' }, Math.round(i.fps ?? 0)), h('td', { 'data-num': '' }, (i.load ?? 0) + '%'),
          h('td', { 'data-num': '' }, i.peak == null ? '–' : i.peak + '%'), h('td', { 'data-num': '' }, i.max == null ? '–' : i.max + '%'),
          h('td', { 'data-num': '' }, i.errors ?? 0),
          h('td', { 'data-num': '' }, i.tx_mode ? h('mark', { 'data-state': 'warn' }, 'TX, ACKing') : i.listen_only ? 'listen-only' : h('mark', { 'data-state': 'warn' }, 'ACKing')));
      })))) : h('p', {}, 'No CAN adapters detected.'),
    s.ifaces.some(i => i.config_error) && h('ul', {}, s.ifaces.filter(i => i.config_error).map(i => h('li', {}, h('small', {}, `${i.name}: ${i.config_error}`)))));
}

async function loadSessions() {
  if (logger.kind !== 'wifi') { sessSec.hidden = true; return; }
  sessSec.hidden = false;
  fill(sessSec, h('h2', {}, h('span', {}, 'Recorded sessions')), h('p', {}, h('small', {}, 'Loading…')));
  let list;
  try {
    list = await sessions();
  } catch (e) {
    fill(sessSec, h('h2', {}, h('span', {}, 'Recorded sessions')), h('p', {}, 'Could not list sessions: ' + e.message));
    return;
  }
  const prog = h('progress', { hidden: true });
  const open = async s => {
    prog.hidden = false;
    prog.removeAttribute('value');
    try {
      const r = await openSession(s, b => (prog.textContent = `${(b / 1e6).toFixed(1)} MB`));
      toast(`Opened session #${s.id}: ${fmtNum(r.frames)} frames`);
      location.hash = '#data';
    } catch (e) { toast(e.message, 6000); }
    prog.hidden = true;
  };
  fill(sessSec,
    h('h2', {}, h('span', {}, 'Recorded sessions'), h('button', { value: 'secondary', onclick: loadSessions }, 'Refresh')),
    prog,
    list.length ? h('figure', {}, h('table', {},
      h('thead', {}, h('tr', {}, h('th', {}, '#'), h('th', {}, 'Name'), h('th', {}, 'Start'), h('th', { 'data-num': '' }, 'Duration'), h('th', { 'data-num': '' }, 'Size'), h('th', {}, ''))),
      h('tbody', {}, list.map(s => h('tr', {},
        h('td', {}, s.id), h('td', {}, s.name || ''), h('td', {}, s.start_utc_ms ? new Date(s.start_utc_ms).toLocaleString() : 'clock not set'),
        h('td', { 'data-num': '' }, fmtDur(s.duration_s)), h('td', { 'data-num': '' }, s.size_kb > 2048 ? (s.size_kb / 1024).toFixed(1) + ' MB' : s.size_kb + ' kB'),
        h('td', {},
          h('button', { value: 'secondary', onclick: () => open(s) }, 'Open in app'), ' ',
          ['candump', 'asc'].map(fmt => h('button', { value: 'secondary', onclick: async e => {
            e.target.disabled = true;
            try {
              download((fmt === 'asc' ? s.file_asc : s.file_candump) + '.gz', await saveSession(s, fmt));
            } catch (err) { toast(err.message, 5000); }
            e.target.disabled = false;
          } }, fmt === 'asc' ? '.asc' : '.log')))))))) : h('p', {}, 'No sessions yet.'));
}
