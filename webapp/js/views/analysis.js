// Analysis tab:
//  - measured bus load (average, peaks over 10 ms / 100 ms / 1 s windows)
//  - measured timing per message (period, jitter, gaps) vs the DBC cycle time
//  - response time from the DBC: worst case (CAN schedulability analysis)
//    and average (simulation), per message, for a chosen bus profile

import * as engine from '../engine.js';
import { dataset } from '../dataset.js';
import * as profiles from '../profiles.js';
import * as store from '../store.js';
import { chart, destroy } from '../chart.js';
import { h, fill, toast, fmt, fmtNum, hexId } from '../ui.js';

let loadSec, timingSec, rtaSec, loadPlot = null, timer = null, visible = false;
let timingBus = '';

export function init(root) {
  loadSec = h('section');
  timingSec = h('section');
  rtaSec = h('section');
  root.append(loadSec, timingSec, rtaSec);
  dataset.addEventListener('change', () => visible && dataset.kind !== 'live' && refresh());
  profiles.changed.addEventListener('change', () => { if (visible) { refresh(); renderRtaForm(); } });
  renderRtaForm();
}

export function show() {
  visible = true;
  refresh();
  renderRtaForm();
  timer = setInterval(() => dataset.kind === 'live' && refresh(), 2000);
}

export function hide() {
  visible = false;
  clearInterval(timer);
}

async function refresh() {
  await renderLoad();
  await renderTiming();
}

// ------------------------------------------------------------------ bus load

async function renderLoad() {
  const s = dataset.summary;
  if (!s?.frames) {
    destroy(loadPlot);
    loadPlot = null;
    fill(loadSec, h('h2', {}, h('span', {}, 'Bus load (measured)')), h('p', {}, 'Open a log or use live data (Data tab) to measure bus load.'));
    return;
  }
  const br = profiles.bitrates();
  const loads = await engine.call('bus_load', JSON.stringify(br));
  // the first and last 1 s windows are partial: leave them out of the chart
  for (const l of loads) if (l.timeline.length > 2) l.timeline = l.timeline.slice(1, -1);
  const noRate = loads.filter(l => !l.bitrate).map(l => l.bus);
  const pct = v => v ? v.toFixed(1) + '%' : '–';
  const st = v => v >= 80 ? 'bad' : v >= 50 ? 'warn' : '';
  let fig = loadSec.querySelector('figure[data-plot]');
  const table = h('figure', {}, h('table', {},
    h('thead', {}, h('tr', {}, ['Bus', 'kbit/s', 'Frames', 'Frames/s', 'Average', 'Peak 10 ms', 'Peak 100 ms', 'Peak 1 s', 'Error frames', 'Sent by us'].map((t, i) => h('th', i ? { 'data-num': '' } : {}, t)))),
    h('tbody', {}, loads.map(l => h('tr', { 'data-state': st(l.peak_pct[1]) },
      h('td', {}, l.bus), h('td', { 'data-num': '' }, l.bitrate ? l.bitrate / 1000 + (l.dbitrate ? '/' + l.dbitrate / 1000 : '') : '?'),
      h('td', { 'data-num': '' }, fmtNum(l.frames)), h('td', { 'data-num': '' }, fmt(l.frames_per_s, 0)),
      h('td', { 'data-num': '' }, pct(l.avg_pct)), h('td', { 'data-num': '' }, pct(l.peak_pct[0])),
      h('td', { 'data-num': '' }, pct(l.peak_pct[1])), h('td', { 'data-num': '' }, pct(l.peak_pct[2])),
      h('td', { 'data-num': '' }, fmtNum(l.error_frames)), h('td', { 'data-num': '' }, fmtNum(l.tx_frames)))))));
  if (!fig) {
    fig = h('figure', { 'data-plot': '' });
    fill(loadSec, h('h2', {}, h('span', {}, 'Bus load (measured)')), table, fig,
      h('p', {}, h('small', {}, 'Frame lengths on the wire are exact: stuff bits are counted from the real payload and CRC, plus the 3-bit interframe space. Peaks are the busiest fixed window of each length. The chart shows load per second.')),
      h('p', { 'data-norate': '' }));
  } else {
    loadSec.querySelector('figure:not([data-plot])').replaceWith(table);
  }
  fill(loadSec.querySelector('p[data-norate]'), noRate.length ? h('small', {}, `No bitrate for ${noRate.join(', ')}: set it in the bus profile (Buses tab).`) : null);
  const withTl = loads.filter(l => l.timeline.length);
  const key = withTl.map(l => l.bus).join('\n') + s.wall_clock;
  if (!loadPlot || loadPlot._key !== key) {
    destroy(loadPlot);
    loadPlot = withTl.length ? chart(fig, { series: withTl.map(l => ({ label: l.bus, unit: '%' })), wall: s.wall_clock, height: 180, step: true }) : null;
    if (loadPlot) loadPlot._key = key;
  }
  if (loadPlot) {
    // align timelines on their common 1 s grid
    const ts = [...new Set(withTl.flatMap(l => l.timeline.map(p => p[0])))].sort((a, b) => a - b);
    const ys = withTl.map(l => { const m = new Map(l.timeline); return ts.map(t => m.get(t) ?? 0); });
    loadPlot.setData([ts, ...ys]);
  }
}

// ------------------------------------------------------------------ timing

async function renderTiming() {
  const buses = dataset.summary?.buses ?? [];
  if (!buses.length) { fill(timingSec); timingSec.hidden = true; return; }
  timingSec.hidden = false;
  if (!buses.includes(timingBus)) timingBus = buses[0];
  const [br, dbr] = profiles.bitrates()[timingBus] ?? [0, 0];
  const rows = await engine.call('msg_stats', timingBus, br, dbr);
  const pid = profiles.profileForBus(timingBus)?.id;
  const dbcMsgs = pid ? await profiles.messages(pid) : [];
  const cyc = new Map(dbcMsgs.map(m => [`${m.ext}:${m.id}`, m.cycle_ms]));
  const sel = h('select', { onchange: () => { timingBus = sel.value; renderTiming(); } }, buses.map(b => h('option', { value: b, selected: b === timingBus }, b)));
  fill(timingSec,
    h('h2', {}, h('span', {}, 'Message timing (measured)'), sel),
    h('figure', {}, h('table', {},
      h('thead', {}, h('tr', {}, ['ID', 'Name', 'Count', 'DBC cycle ms', 'Mean period ms', 'Jitter ms', 'Min ms', 'Max gap ms', 'Bits', 'Load'].map((t, i) => h('th', i > 1 ? { 'data-num': '' } : {}, t)))),
      h('tbody', {}, rows.map(r => {
        const c = cyc.get(`${r.ext}:${r.id}`);
        const late = c && r.count > 2 && r.max_ms > 1.5 * c;
        return h('tr', { 'data-state': late ? 'warn' : '' },
          h('td', {}, h('code', {}, hexId(r.id, r.ext))), h('td', {}, r.name ?? ''),
          h('td', { 'data-num': '' }, fmtNum(r.count)), h('td', { 'data-num': '' }, c ?? ''),
          h('td', { 'data-num': '' }, r.count > 1 ? fmt(r.period_ms, 2) : ''), h('td', { 'data-num': '' }, r.count > 2 ? fmt(r.jitter_ms, 3) : ''),
          h('td', { 'data-num': '' }, r.count > 1 ? fmt(r.min_ms, 2) : ''), h('td', { 'data-num': '' }, r.count > 1 ? fmt(r.max_ms, 2) : ''),
          h('td', { 'data-num': '' }, fmt(r.mean_bits, 1)), h('td', { 'data-num': '' }, br ? fmt(r.load_pct, 2) + '%' : ''));
      })))),
    h('p', {}, h('small', {}, 'Rows in orange: the largest gap is over 1.5× the DBC cycle time. A log only shows when frames finished, not when they were queued, so this is not the response time; that comes from the DBC below.')));
}

// ------------------------------------------------------------------ response time

let rtaProfile = '', lastReport = null;

async function overrides(pid) {
  return (await store.setting('rta_overrides', {}))[pid] ?? {};
}

async function setOverride(pid, key, period) {
  const all = await store.setting('rta_overrides', {});
  all[pid] = { ...(all[pid] ?? {}) };
  if (period > 0) all[pid][key] = period; else delete all[pid][key];
  await store.setSetting('rta_overrides', all);
}

function renderRtaForm() {
  const ps = profiles.ws.profiles;
  if (!ps.some(p => p.id === rtaProfile)) rtaProfile = ps.find(p => p.dbcs.length)?.id ?? ps[0]?.id ?? '';
  const sel = h('select', { onchange: () => { rtaProfile = sel.value; lastReport = null; renderRtaForm(); } },
    ps.map(p => h('option', { value: p.id, selected: p.id === rtaProfile }, p.name)));
  const p = ps.find(x => x.id === rtaProfile);
  const simS = h('input', { type: 'number', value: 10, min: 1, max: 120, step: 1, 'aria-label': 'Simulated seconds' });
  const out = h('div');
  const run = async () => {
    if (!p?.bitrate) return toast('Set the bitrate of this bus profile first (Buses tab)');
    fill(out, h('progress'));
    try {
      const ov = await overrides(p.id);
      const list = Object.entries(ov).map(([k, period]) => { const [ext, id] = k.split(':'); return { id: +id, ext: ext === 'true', period_ms: period }; });
      lastReport = await engine.call('rta', profiles.engineBus(p.id), JSON.stringify({ bitrate: p.bitrate, dbitrate: profiles.dataBitrate(p), sim_seconds: +simS.value || 10, sim_runs: 4 }), JSON.stringify(list));
      renderRta(out, p, ov);
    } catch (e) { fill(out, h('p', {}, 'Analysis failed: ' + e.message)); }
  };
  fill(rtaSec,
    h('h2', {}, h('span', {}, 'Response time (from the DBC)')),
    !ps.length ? h('p', {}, 'Create a bus profile with a DBC in the ', h('a', { href: '#buses' }, 'Buses'), ' tab.') : [
      h('form', { onsubmit: e => { e.preventDefault(); run(); } },
        h('label', {}, 'Bus ', sel),
        h('label', {}, 'Simulate ', simS, ' s'),
        h('button', {}, 'Analyse')),
      p && !p.dbcs.length && h('p', {}, 'This profile has no DBC.'),
      h('details', {}, h('summary', {}, 'What is computed'),
        h('p', {}, h('small', {}, 'Response time = from the moment a message is queued in its sender to the end of its transmission. Priorities come from the CAN ids, frame sizes from the DBC, periods from GenMsgCycleTime.')),
        h('ul', {},
          h('li', {}, h('small', {}, h('b', {}, 'Worst case: '), 'CAN schedulability analysis (Davis, Burns, Bril, Lukkien 2007): longest blocking by one lower-priority frame, every higher-priority message queued at the same time, worst-case bit stuffing, no bus errors.')),
          h('li', {}, h('small', {}, h('b', {}, 'Average and max seen: '), 'a simulation of the bus with random start phases, frame lengths from random payloads (or the real mean length when the bus is in the loaded data).')),
          h('li', {}, h('small', {}, 'Event messages without a cycle time are left out of the interference unless you give them a minimum gap (column "Period").')))),
      out,
    ]);
  if (lastReport && p) overrides(p.id).then(ov => renderRta(out, p, ov));
}

function renderRta(out, p, ov) {
  const r = lastReport;
  const ms = v => v == null ? '∞' : v < 1 ? v.toFixed(3) : v.toFixed(2);
  const bad = r.messages.filter(m => m.schedulable === false).length;
  fill(out,
    h('dl', {},
      h('div', {}, h('dt', {}, 'utilisation, worst-case stuffing'), h('dd', { style: r.util_worst_pct >= 100 ? 'color:var(--bad)' : '' }, r.util_worst_pct.toFixed(1) + '%')),
      h('div', {}, h('dt', {}, 'utilisation, average'), h('dd', {}, r.util_avg_pct.toFixed(1) + '%')),
      h('div', {}, h('dt', {}, 'messages'), h('dd', {}, r.messages.length)),
      h('div', {}, h('dt', {}, 'missing deadline'), h('dd', { style: bad ? 'color:var(--bad)' : '' }, bad)),
      h('div', {}, h('dt', {}, 'no period'), h('dd', {}, r.unknown_period))),
    h('figure', {}, h('table', {},
      h('thead', {}, h('tr', {}, ['ID', 'Name', 'Period ms', 'Frame ms (worst / avg)', 'Blocking ms', 'Worst R ms', 'Average R ms', 'Max seen ms', 'Slack ms', ''].map((t, i) => h('th', i > 1 && i < 9 ? { 'data-num': '' } : {}, t)))),
      h('tbody', {}, r.messages.map(m => {
        const key = `${m.ext}:${m.id}`;
        const per = h('input', { type: 'number', min: 0, step: 'any', value: ov[key] ?? '', placeholder: m.period_ms ?? 'none', style: 'width:6em', title: 'Period or minimum gap (ms) for this analysis' });
        per.onchange = () => setOverride(p.id, key, +per.value);
        const slack = m.deadline_ms != null && m.r_worst_ms != null ? m.deadline_ms - m.r_worst_ms : null;
        return h('tr', { 'data-state': m.schedulable === false ? 'bad' : slack != null && slack < 0.2 * m.deadline_ms ? 'warn' : '' },
          h('td', {}, h('code', {}, hexId(m.id, m.ext))), h('td', {}, m.name),
          h('td', { 'data-num': '' }, per),
          h('td', { 'data-num': '' }, `${ms(m.c_worst_ms)} / ${ms(m.c_avg_ms)}`),
          h('td', { 'data-num': '' }, ms(m.blocking_ms)),
          h('td', { 'data-num': '' }, ms(m.r_worst_ms)),
          h('td', { 'data-num': '' }, m.r_avg_ms == null ? '–' : ms(m.r_avg_ms)),
          h('td', { 'data-num': '' }, m.r_sim_max_ms == null ? '–' : ms(m.r_sim_max_ms)),
          h('td', { 'data-num': '' }, slack == null ? '–' : ms(slack)),
          h('td', {}, m.schedulable === false ? h('mark', { 'data-state': 'bad' }, 'misses deadline') : m.schedulable ? h('mark', { 'data-state': 'ok' }, 'ok') : h('mark', {}, 'no deadline')));
      })))),
    h('p', {}, h('small', {}, `Ordered by priority (lowest id first). ${p.bitrate / 1000} kbit/s${profiles.dataBitrate(p) ? `, CAN FD data phase ${profiles.dataBitrate(p) / 1000} kbit/s (from the DBC)` : ''}. Deadline = period. Change a period and press Analyse again.`)));
}
