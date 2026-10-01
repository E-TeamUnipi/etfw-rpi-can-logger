// Send tab: one-shot frames on a logger bus, built from the DBC (signal
// values) or raw. The first frame on a bus switches its adapter out of
// listen-only; it stays in TX mode (ACKing frames) until switched back here.

import * as engine from '../engine.js';
import * as profiles from '../profiles.js';
import * as store from '../store.js';
import { logger, command, busName } from '../logger.js';
import { h, fill, toast, ask, hexId, hexData, parseHex } from '../ui.js';

let root, top, form, hist, ifaceName = '', mode = 'dbc', msgName = '';
const history = [];
const values = {};  // msgName -> {signal: value}

export function init(el) {
  root = el;
  top = h('section');
  form = h('section');
  hist = h('section', { hidden: true });
  root.append(top, form, hist);
  logger.addEventListener('status', () => root.hidden || renderTop());
  logger.addEventListener('state', () => root.hidden || render());
  profiles.changed.addEventListener('change', () => root.hidden || renderForm());
}

export function show() {
  render();
}

function render() {
  renderTop();
  renderForm();
}

const ifaces = () => logger.status?.ifaces ?? [];
const iface = () => ifaces().find(i => i.name === ifaceName);

function renderTop() {
  if (logger.state !== 'connected') {
    fill(top, h('h2', {}, h('span', {}, 'Send')), h('p', {}, 'Connect to the logger first (Logger tab). Sending works over Wi-Fi and Bluetooth.'));
    form.hidden = true;
    return;
  }
  form.hidden = false;
  const list = ifaces();
  if (!list.some(i => i.name === ifaceName)) ifaceName = (list.find(i => i.tx_allowed) ?? list[0])?.name ?? '';
  const sel = h('select', { onchange: () => { ifaceName = sel.value; render(); } },
    list.map(i => h('option', { value: i.name, selected: i.name === ifaceName }, `${busName(i)} (${i.name})${i.tx_allowed ? '' : ' – sending not enabled'}`)));
  const pin = h('input', { type: 'password', value: logger.pin, placeholder: 'PIN', size: 8, autocomplete: 'off', inputmode: 'numeric', onchange: e => { logger.pin = e.target.value.trim(); store.setSetting('pin', logger.pin); } });
  const i = iface();
  fill(top,
    h('h2', {}, h('span', {}, 'Send on')),
    h('form', { onsubmit: e => e.preventDefault() }, sel, h('label', {}, 'PIN ', pin)),
    i && !i.tx_allowed && h('p', {}, h('mark', { 'data-state': 'warn' }, 'disabled'), ' Sending is not enabled on this adapter. Set ', h('code', {}, `can.${i.serial || i.name}.tx = 1`), ' in logger.conf, and a ', h('code', {}, 'control_pin'), '.'),
    i?.tx_mode
      ? h('p', {}, h('mark', { 'data-state': 'warn' }, 'TX mode'), ' This adapter is out of listen-only: it ACKs every frame on the bus. ',
          h('button', { value: 'secondary', onclick: () => command({ cmd: 'tx_mode', iface: i.name, on: false }).then(() => toast('Back to listen-only'), e => toast(e.message, 5000)) }, 'Back to listen-only'))
      : i?.tx_allowed && h('p', {}, h('small', {}, 'The first frame you send switches this adapter out of listen-only (it will ACK frames, one-shot transmission). It stays that way until you switch it back or the logger restarts.')));
}

function renderForm() {
  if (logger.state !== 'connected') return;
  const i = iface();
  const p = i ? profiles.profileForBus(busName(i)) : null;
  const tabs = h('fieldset', {}, h('legend', {}, 'Frame'),
    ['dbc', 'raw'].map(m => h('label', {}, h('input', { type: 'radio', name: 'sendmode', checked: mode === m, onchange: () => { mode = m; renderForm(); } }), m === 'dbc' ? 'From DBC' : 'Raw')));
  const body = h('div');
  fill(form, h('h2', {}, h('span', {}, 'Frame')), tabs, body);
  if (mode === 'raw') return renderRaw(body);
  if (!p?.dbcs.length) {
    fill(body, h('p', {}, `No DBC for ${i ? busName(i) : 'this bus'}: create a bus profile named “${i ? busName(i) : ''}” with its DBC in the `, h('a', { href: '#buses' }, 'Buses'), ' tab, or send a raw frame.'));
    return;
  }
  renderDbc(body, p);
}

function renderRaw(body) {
  const id = h('input', { type: 'text', placeholder: 'ID hex, e.g. 123', size: 10, pattern: '[0-9a-fA-F]{1,8}', required: true });
  const ext = h('input', { type: 'checkbox' });
  const fd = h('input', { type: 'checkbox' });
  const brs = h('input', { type: 'checkbox', checked: true });
  const data = h('input', { type: 'text', placeholder: 'Data hex, e.g. 01 02 03', size: 30 });
  fill(body, h('form', { onsubmit: e => {
    e.preventDefault();
    const bytes = parseHex(data.value);
    if (!bytes) return toast('Data must be hex bytes');
    send({ id: parseInt(id.value, 16), ext: ext.checked, fd: fd.checked, brs: fd.checked && brs.checked, data: bytes, label: 'raw' });
  } },
    id, h('label', {}, ext, 'extended'), h('label', {}, fd, 'CAN FD'), h('label', {}, brs, 'BRS'), data, h('button', {}, 'Send')));
}

async function renderDbc(body, p) {
  const msgs = await profiles.messages(p.id);
  if (!msgs.some(m => m.name === msgName)) msgName = msgs[0]?.name ?? '';
  const filter = h('input', { type: 'search', placeholder: 'Find message', size: 16 });
  const sel = h('select', { onchange: () => { msgName = sel.value; renderDbc(body, p); } });
  const fillSel = () => fill(sel, msgs.filter(m => !filter.value || m.name.toLowerCase().includes(filter.value.toLowerCase()) || hexId(m.id, m.ext).includes(filter.value.toUpperCase()))
    .map(m => h('option', { value: m.name, selected: m.name === msgName }, `${m.name} (0x${hexId(m.id, m.ext)})`)));
  filter.oninput = fillSel;
  fillSel();
  const m = msgs.find(x => x.name === msgName);
  if (!m) return fill(body, h('p', {}, 'This DBC has no messages.'));
  const v = (values[m.name] ??= {});
  const preview = h('code');
  const update = async () => {
    try {
      const bytes = await engine.call('encode', profiles.engineBus(p.id), m.name, JSON.stringify(v));
      preview.textContent = hexData(bytes);
      return bytes;
    } catch (e) { preview.textContent = e.message; }
  };
  const rows = m.signals.map(s => {
    const def = s.start_raw != null ? s.start_raw * s.factor + s.offset : 0;
    const input = s.choices.length
      ? h('select', {}, s.choices.map(([raw, text]) => h('option', { value: raw * s.factor + s.offset, selected: (v[s.name] ?? def) === raw * s.factor + s.offset }, `${raw}: ${text}`)))
      : h('input', { type: 'number', step: 'any', value: v[s.name] ?? def, min: s.min < s.max ? s.min : undefined, max: s.min < s.max ? s.max : undefined, style: 'width:9em' });
    input.oninput = input.onchange = () => { v[s.name] = +input.value; update(); };
    return h('tr', {}, h('td', {}, s.name, s.mux ? h('small', {}, ` (${s.mux})`) : ''), h('td', {}, input), h('td', {}, s.unit),
      h('td', {}, h('small', {}, s.min < s.max ? `${s.min} … ${s.max}` : '')), h('td', {}, h('small', {}, s.comment)));
  });
  fill(body,
    h('form', { onsubmit: e => e.preventDefault() }, filter, sel),
    h('p', {}, h('small', {}, `0x${hexId(m.id, m.ext)}${m.ext ? ' extended' : ''}, ${m.size} bytes${m.fd ? ', CAN FD' + (m.brs ? ' with BRS' : '') : ''}${m.cycle_ms ? `, cycle ${m.cycle_ms} ms` : ''}${m.sender ? `, sent by ${m.sender}` : ''}. Multiplexed signals are written only when their multiplexor value selects them.`)),
    h('figure', {}, h('table', {}, h('thead', {}, h('tr', {}, ['Signal', 'Value', 'Unit', 'Range', ''].map(t => h('th', {}, t)))), h('tbody', {}, rows))),
    h('form', { onsubmit: async e => {
      e.preventDefault();
      const bytes = await update();
      if (bytes) send({ id: m.id, ext: m.ext, fd: m.fd, brs: m.brs, data: bytes, label: m.name });
    } }, h('span', {}, 'Payload: ', preview), h('button', {}, 'Send')));
  update();
}

async function send({ id, ext, fd, brs, data, label }) {
  const i = iface();
  if (!i) return;
  if (!i.tx_mode && !(await ask(`Send on ${busName(i)}? The adapter leaves listen-only and will ACK frames on this bus until you switch it back.`, 'Send'))) return;
  const entry = { t: new Date(), bus: busName(i), id, ext, data, label, result: '…' };
  history.unshift(entry);
  try {
    const r = await command({ cmd: 'send', iface: i.name, id, ext, fd, brs, data: hexData(data).replaceAll(' ', '') });
    entry.result = r.pending ? 'sent over Bluetooth (result in logger status)' : r.tx_mode_changed ? 'sent (TX mode on)' : 'sent';
  } catch (e) {
    entry.result = 'failed: ' + e.message;
    toast(e.message, 6000);
  }
  renderHistory();
}

function renderHistory() {
  hist.hidden = !history.length;
  fill(hist, h('h2', {}, h('span', {}, 'Sent')),
    h('figure', {}, h('table', {}, h('tbody', {}, history.slice(0, 50).map(e => h('tr', { 'data-state': e.result.startsWith('failed') ? 'bad' : '' },
      h('td', {}, e.t.toLocaleTimeString()), h('td', {}, e.bus), h('td', {}, h('code', {}, hexId(e.id, e.ext))), h('td', {}, e.label),
      h('td', {}, h('code', {}, hexData(e.data))), h('td', {}, e.result)))))),
    h('p', {}, h('small', {}, 'Sent frames are recorded in the log like any other frame, marked as transmitted (Tx in .asc exports).')));
}
