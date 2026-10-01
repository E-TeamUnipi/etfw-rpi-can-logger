// Plots tab: workbooks of plot panels. A workbook is a named list of panels,
// each with signals (bus profile + message + signal), saved in this browser.
// The same workbook plots live data (scrolling window) and loaded logs
// (whole recording; drag to zoom, double-click to zoom out).

import * as engine from '../engine.js';
import { dataset } from '../dataset.js';
import * as profiles from '../profiles.js';
import * as store from '../store.js';
import { chart, destroy, align } from '../chart.js';
import { h, fill, toast, ask, download, debounce } from '../ui.js';

let root, bar, panelsEl, wb = null, books = [], visible = false, timer = null;
let range = null;            // [t0, t1] zoom (plot seconds), null = everything / live window
let liveWindow = 30;         // s
const charts = new Map();    // panel id -> uPlot

export function init(el) {
  root = el;
  bar = h('section');
  panelsEl = h('div', { style: 'display:grid;gap:12px' });
  root.append(bar, panelsEl);
  dataset.addEventListener('change', () => { if (dataset.kind !== 'live') { range = null; } if (visible) { renderBar(); redraw(); } });
  profiles.changed.addEventListener('change', () => visible && redraw());
  load();
}

async function load() {
  books = (await store.all('workbooks')).sort((a, b) => a.name.localeCompare(b.name));
  const lastId = await store.setting('workbook', null);
  wb = books.find(b => b.id === lastId) ?? books[0] ?? null;
  liveWindow = await store.setting('live_window', 30);
  if (visible) render();
}

export function show() {
  visible = true;
  render();
  timer = setInterval(() => dataset.kind === 'live' && !range && redraw(), 500);
}

export function hide() {
  visible = false;
  clearInterval(timer);
}

const save = debounce(() => wb && store.put('workbooks', wb), 300);

function render() {
  renderBar();
  renderPanels();
}

function renderBar() {
  const sel = h('select', { onchange: () => { wb = books.find(b => b.id === sel.value); store.setSetting('workbook', wb?.id); range = null; render(); } },
    books.map(b => h('option', { value: b.id, selected: b.id === wb?.id }, b.name)));
  const newBook = async () => {
    const b = { id: store.newId(), name: `Workbook ${books.length + 1}`, panels: [{ id: store.newId(), title: 'Panel 1', signals: [] }] };
    await store.put('workbooks', b);
    books.push(b);
    wb = b;
    store.setSetting('workbook', b.id);
    render();
  };
  const imp = h('input', { type: 'file', accept: '.json', hidden: true });
  imp.onchange = async () => {
    try {
      const b = JSON.parse(await imp.files[0].text());
      if (!Array.isArray(b.panels)) throw new Error('not a workbook');
      b.id = store.newId();
      await store.put('workbooks', b);
      books.push(b);
      wb = b;
      render();
    } catch (e) { toast('Import failed: ' + e.message, 5000); }
  };
  const live = dataset.kind === 'live';
  const win = h('select', { onchange: () => { liveWindow = +win.value; store.setSetting('live_window', liveWindow); redraw(); } },
    [10, 30, 60, 300, 900].map(s => h('option', { value: s, selected: s === liveWindow }, s < 60 ? `${s} s` : `${s / 60} min`)));
  fill(bar,
    h('h2', {}, h('span', {}, 'Workbook'),
      books.length > 0 && sel,
      h('button', { value: 'secondary', onclick: newBook }, 'New'),
      wb && h('button', { value: 'secondary', onclick: () => {
        const name = bar.querySelector('input[name=wbname]');
        name.hidden = !name.hidden;
        name.focus();
      } }, 'Rename'),
      wb && h('button', { value: 'secondary', onclick: () => download(`${wb.name}.workbook.json`, new Blob([JSON.stringify(wb, null, 1)], { type: 'application/json' })) }, 'Export'),
      h('button', { value: 'secondary', onclick: () => imp.click() }, 'Import'), imp,
      wb && h('button', { value: 'secondary', onclick: async () => {
        if (!(await ask(`Delete workbook "${wb.name}"?`, 'Delete'))) return;
        await store.del('workbooks', wb.id);
        books = books.filter(b => b !== wb);
        wb = books[0] ?? null;
        render();
      } }, 'Delete')),
    wb && h('input', { type: 'text', name: 'wbname', value: wb.name, hidden: true, onchange: e => { wb.name = e.target.value || wb.name; save(); renderBar(); } }),
    !wb ? h('p', {}, 'A workbook is a set of plots of DBC signals, saved in this browser. ', h('button', { onclick: newBook }, 'Create one')) :
      h('form', { onsubmit: e => e.preventDefault() },
        live ? h('label', {}, 'Live window ', win) : null,
        range ? h('button', { type: 'button', value: 'secondary', onclick: () => { range = null; redraw(); } }, 'Zoom out') : null,
        h('button', { type: 'button', value: 'secondary', onclick: () => { wb.panels.push({ id: store.newId(), title: `Panel ${wb.panels.length + 1}`, signals: [] }); save(); renderPanels(); } }, 'Add panel'),
        h('small', {}, dataset.summary?.frames ? (live ? 'Live: drag on a plot to pause and zoom in.' : 'Drag to zoom in, double-click to zoom out.') : 'No data loaded: plots fill when you open a log or use live data.')));
}

function renderPanels() {
  for (const u of charts.values()) destroy(u);
  charts.clear();
  if (!wb) return fill(panelsEl);
  fill(panelsEl, wb.panels.map(p => {
    const fig = h('figure', { 'data-plot': '' });
    const title = h('input', { type: 'text', value: p.title, 'aria-label': 'Panel title', onchange: e => { p.title = e.target.value; save(); } });
    const sec = h('section', {},
      h('header', {}, h('h3', {}, title),
        h('button', { value: 'secondary', onclick: () => pickSignal(p) }, '+ Signal'),
        h('button', { value: 'secondary', title: 'Remove panel', onclick: () => { wb.panels = wb.panels.filter(x => x !== p); save(); renderPanels(); } }, '×')),
      p.signals.length ? h('p', { 'data-chips': '' }, p.signals.map((s, i) => h('mark', { title: `${profiles.profileName(s.pid)} · ${s.msg}.${s.sig}` }, s.sig,
        h('button', { title: 'Remove signal', onclick: () => { p.signals.splice(i, 1); save(); renderPanels(); } }, '×')))) :
        h('p', {}, h('small', {}, 'No signals yet: add one with “+ Signal”.')),
      fig);
    if (p.signals.length) {
      charts.set(p.id, chart(fig, {
        series: p.signals.map(s => ({ label: s.sig, unit: s.unit ?? '' })),
        wall: !!dataset.summary?.wall_clock, height: 240, sync: 'wb',
        step: true,
        onZoom: (a, b) => { range = a == null ? null : [a, b]; renderBar(); redraw(); },
      }));
    }
    return sec;
  }));
  redraw();
}

let drawing = false;
async function redraw() {
  if (!wb || drawing || !visible) return;
  drawing = true;
  try {
    const s = dataset.summary;
    let t0, t1;
    if (range) [t0, t1] = range;
    else if (dataset.kind === 'live' && s?.wall_clock) { t1 = Date.now() / 1000; t0 = t1 - liveWindow; }
    else if (s?.frames) { t0 = s.start_s; t1 = s.end_s; }
    else { t0 = 0; t1 = 1; }
    for (const p of wb.panels) {
      const u = charts.get(p.id);
      if (!u) continue;
      const px = Math.max(100, Math.round(u.width));
      const data = [];
      for (const sg of p.signals) {
        const bus = profiles.busForProfile(sg.pid);
        data.push(bus ? await engine.call('series', bus, sg.msg, sg.sig, t0, t1, px) : new Float64Array(0));
      }
      u.setData(align(data));
      u.setScale('x', { min: t0, max: t1 });
    }
  } catch (e) {
    console.warn('plot', e);
  } finally {
    drawing = false;
  }
}

let lastPid = null;

/** Signal picker: tick any number of signals (across searches and bus
 *  profiles), then add them all at once. */
async function pickSignal(panel) {
  const ps = profiles.ws.profiles.filter(p => p.dbcs.length);
  if (!ps.length) return toast('Add a bus profile with a DBC first (Buses tab)', 4000);
  let pid = ps.find(p => p.id === lastPid)?.id ?? ps.find(p => profiles.busForProfile(p.id))?.id ?? ps[0].id;
  const key = (pid, msg, sig) => `${pid}\n${msg}\n${sig}`;
  const have = new Set(panel.signals.map(s => key(s.pid, s.msg, s.sig)));
  const picked = new Map();   // key -> signal
  const list = h('div', { style: 'max-height:50vh;overflow:auto' });
  const q = h('input', { type: 'search', placeholder: 'Search message or signal', autofocus: true });
  const psel = h('select', {}, ps.map(p => h('option', { value: p.id, selected: p.id === pid }, p.name + (profiles.busForProfile(p.id) ? '' : ' (no data)'))));
  const add = h('button', { value: 'add', disabled: true }, 'Add');
  const count = () => {
    add.disabled = !picked.size;
    add.textContent = picked.size > 1 ? `Add ${picked.size} signals` : 'Add';
  };
  const d = h('dialog', { style: 'width:min(560px,calc(100vw - 32px))' },
    h('form', { method: 'dialog' }, psel, q, list,
      h('menu', {}, h('button', { value: 'cancel', formnovalidate: true }, 'Cancel'), add)));
  let msgs = [];
  const renderList = () => {
    const f = q.value.trim().toLowerCase();
    fill(list, msgs.map(m => {
      const sigs = m.signals.filter(s => !f || m.name.toLowerCase().includes(f) || s.name.toLowerCase().includes(f));
      if (!sigs.length) return null;
      const boxes = sigs.map(s => {
        const k = key(pid, m.name, s.name);
        const sig = { pid, msg: m.name, sig: s.name, unit: s.unit };
        return h('li', {}, h('label', {},
          h('input', { type: 'checkbox', checked: have.has(k) || picked.has(k), disabled: have.has(k),
            onchange: e => { e.target.checked ? picked.set(k, sig) : picked.delete(k); count(); } }),
          s.name, ' ', h('small', {}, [s.unit, s.comment].filter(Boolean).join(' · '))));
      });
      return h('details', { open: !!f || msgs.length < 6 }, h('summary', {}, `${m.name} `, h('small', {}, `0x${m.id.toString(16).toUpperCase()}`)),
        h('ul', { style: 'list-style:none;padding-left:4px' }, boxes));
    }).slice(0, 300));
  };
  const loadMsgs = async () => { msgs = await profiles.messages(pid); renderList(); };
  psel.onchange = () => { pid = psel.value; loadMsgs(); };
  q.oninput = renderList;
  document.body.append(d);
  d.addEventListener('close', () => {
    d.remove();
    if (d.returnValue !== 'add' || !picked.size) return;
    panel.signals.push(...picked.values());
    lastPid = pid;
    save();
    renderPanels();
  });
  d.showModal();
  loadMsgs();
}
