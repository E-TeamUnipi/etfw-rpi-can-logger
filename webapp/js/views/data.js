// Data tab: open a log file (or use live data), map its buses to profiles,
// and browse the latest frame of every message, decoded.

import * as engine from '../engine.js';
import { dataset, openFile, startLive, clear } from '../dataset.js';
import * as profiles from '../profiles.js';
import { logger } from '../logger.js';
import { h, fill, toast, fmtNum, hexId, hexData, timeLabel, fmtDur } from '../ui.js';

let srcSec, mapSec, msgSec, busSel, filterIn, tbody, timer = null, visible = false;
const open = new Set(); // expanded message rows

export function init(root) {
  srcSec = h('section');
  mapSec = h('section', { hidden: true });
  busSel = h('select', { onchange: renderMessages, 'aria-label': 'Bus' });
  filterIn = h('input', { type: 'search', placeholder: 'Filter id or name', oninput: renderMessages, size: 16 });
  tbody = h('tbody');
  msgSec = h('section', { hidden: true },
    h('h2', {}, h('span', {}, 'Messages'), busSel, filterIn),
    h('figure', {}, h('table', {},
      h('thead', {}, h('tr', {}, h('th', {}, 'ID'), h('th', {}, 'Name'), h('th', { 'data-num': '' }, 'Count'), h('th', { 'data-num': '' }, 'Hz'), h('th', {}, 'Data'), h('th', { 'data-num': '' }, 'Signals'))),
      tbody)));
  root.append(srcSec, mapSec, msgSec);
  dataset.addEventListener('change', render);
  profiles.changed.addEventListener('change', () => { renderMap(); renderMessages(); });
  logger.addEventListener('state', renderSource);
  render();
}

export function show() {
  visible = true;
  renderMessages();
  timer = setInterval(() => dataset.kind === 'live' && renderMessages(), 1000);
}

export function hide() {
  visible = false;
  clearInterval(timer);
}

function render() {
  renderSource();
  renderMap();
  const buses = dataset.summary?.buses ?? [];
  const cur = busSel.value;
  fill(busSel, buses.map(b => h('option', { value: b, selected: b === cur }, b)));
  msgSec.hidden = !buses.length;
  renderMessages();
}

function renderSource() {
  const s = dataset.summary;
  const input = h('input', { type: 'file', accept: '.log,.asc,.gz,.txt', 'data-drop': true, hidden: true });
  const prog = h('progress', { hidden: true });
  const load = async file => {
    if (!file) return;
    prog.hidden = false;
    prog.removeAttribute('value');
    try {
      const r = await openFile(file, b => (prog.title = `${(b / 1e6).toFixed(0)} MB read`));
      toast(`${fmtNum(r.frames)} frames from ${file.name}` + (r.bad_lines ? `, ${r.bad_lines} unreadable lines` : ''), 5000);
    } catch (e) { toast('Import failed: ' + e.message, 8000); }
    prog.hidden = true;
  };
  input.onchange = () => load(input.files[0]);
  const drop = h('label', {
    ondragover: e => { e.preventDefault(); drop.dataset.over = ''; },
    ondragleave: () => delete drop.dataset.over,
    ondrop: e => { e.preventDefault(); delete drop.dataset.over; load(e.dataTransfer.files[0]); },
  }, input, h('b', {}, 'Open a log file'), h('small', {}, 'candump .log or Vector .asc, optionally .gz. Drop it here or tap to choose. The file stays on this device.'));
  const imp = dataset.import;
  fill(srcSec,
    h('h2', {}, h('span', {}, 'Data'),
      dataset.kind && h('button', { value: 'secondary', onclick: clear }, 'Close')),
    s?.frames ? h('dl', {},
      h('div', {}, h('dt', {}, 'source'), h('dd', { title: dataset.name ?? '' }, dataset.name || '–')),
      h('div', {}, h('dt', {}, 'frames'), h('dd', {}, fmtNum(s.frames))),
      h('div', {}, h('dt', {}, 'buses'), h('dd', {}, s.buses.length)),
      h('div', {}, h('dt', {}, 'duration'), h('dd', {}, fmtDur(s.end_s - s.start_s))),
      h('div', {}, h('dt', {}, 'start'), h('dd', {}, timeLabel(s.start_s, s.wall_clock))),
      h('div', {}, h('dt', {}, 'memory'), h('dd', {}, `${s.memory_mb.toFixed(0)} MB`)))
      : dataset.kind === 'live' ? h('p', {}, 'Waiting for frames…') : null,
    imp?.bad_lines ? h('p', {}, h('mark', { 'data-state': 'warn' }, `${imp.bad_lines} lines skipped`), ' ', h('small', {}, imp.first_error)) : null,
    s?.markers?.length ? h('details', {}, h('summary', {}, `${s.markers.length} markers`),
      h('ul', {}, s.markers.map(([t, text]) => h('li', {}, `${timeLabel(t, s.wall_clock)}: ${text}`)))) : null,
    prog,
    drop,
    logger.state === 'connected' && dataset.kind !== 'live' &&
      h('form', {}, h('button', { type: 'button', value: 'secondary', onclick: () => startLive().catch(e => toast(e.message)) }, 'Use live data from the logger')));
}

function renderMap() {
  const buses = dataset.summary?.buses ?? [];
  mapSec.hidden = !buses.length;
  if (!buses.length) return;
  const ps = profiles.ws.profiles;
  fill(mapSec,
    h('h2', {}, h('span', {}, 'Bus profiles')),
    h('p', {}, h('small', {}, 'Each bus in this data is decoded with the DBCs of a profile (Buses tab). A profile with the same name as the bus is picked automatically.')),
    h('figure', {}, h('table', {},
      h('thead', {}, h('tr', {}, h('th', {}, 'Bus in data'), h('th', {}, 'Profile'), h('th', {}, 'DBCs'))),
      h('tbody', {}, buses.map(b => {
        const p = profiles.profileForBus(b);
        const sel = h('select', { onchange: () => profiles.mapBus(b, sel.value) },
          h('option', { value: '' }, '— none —'),
          ps.map(x => h('option', { value: x.id, selected: p?.id === x.id }, x.name)));
        return h('tr', {}, h('td', {}, b), h('td', {}, sel),
          h('td', {}, p ? p.dbcs.map(profiles.dbcLabel).join(', ') || 'no DBC' : '–'));
      })))),
    !ps.length && h('p', {}, 'No profiles yet: create one in the ', h('a', { href: '#buses' }, 'Buses'), ' tab and add its DBC.'));
}

async function renderMessages() {
  if (!visible || !busSel.value) return;
  const rows = await engine.call('latest', busSel.value);
  const f = filterIn.value.trim().toLowerCase();
  const shown = rows.filter(r => !f || hexId(r.id, r.ext).toLowerCase().includes(f) || r.name?.toLowerCase().includes(f));
  const live = dataset.kind === 'live';
  const now = Date.now() / 1000;
  // one row per message; an open message gets a full-width row of its signals
  fill(tbody, shown.slice(0, 800).map(r => {
    const key = `${r.ext}:${r.id}`;
    const isOpen = open.has(key);
    const n = r.signals.length;
    const row = h('tr', { 'data-state': live && now - r.t > 3 ? 'stale' : '', 'aria-expanded': n ? String(isOpen) : null,
        onclick: n ? () => { isOpen ? open.delete(key) : open.add(key); renderMessages(); } : null },
      h('td', {}, h('code', {}, hexId(r.id, r.ext))), h('td', {}, r.name ?? ''),
      h('td', { 'data-num': '' }, fmtNum(r.count)), h('td', { 'data-num': '' }, r.hz ? r.hz.toFixed(1) : ''),
      h('td', {}, h('code', {}, hexData(r.data))),
      h('td', { 'data-num': '' }, n || ''));
    if (!isOpen || !n) return row;
    return [row, h('tr', { 'data-detail': '' }, h('td', { colspan: 6 }, h('ul', {}, r.signals.map(s => h('li', {},
      h('span', { title: s.name }, s.name),
      h('b', {}, Number.isInteger(s.value) ? s.value : s.value.toPrecision(6)),
      h('small', {}, [s.unit, s.text].filter(Boolean).join(' · ')))))))];
  }));
}
