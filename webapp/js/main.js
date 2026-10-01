// App shell: tabs (one <article> per view, chosen by the URL hash), the
// connection badge, and start-up.

import { logger } from './logger.js';
import * as profiles from './profiles.js';
import * as store from './store.js';
import { $, toast } from './ui.js';
import * as loggerView from './views/logger.js';
import * as dataView from './views/data.js';
import * as busesView from './views/buses.js';
import * as analysisView from './views/analysis.js';
import * as plotsView from './views/plots.js';
import * as sendView from './views/send.js';

const views = { logger: loggerView, data: dataView, buses: busesView, analysis: analysisView, plots: plotsView, send: sendView };
let current = null;

function route() {
  const name = location.hash.slice(1) in views ? location.hash.slice(1) : 'logger';
  if (current === name) return;
  if (current) {
    $('#view-' + current).hidden = true;
    views[current].hide?.();
  }
  const first = current == null;
  current = name;
  $('#view-' + name).hidden = false;
  if (!first) scrollTo(0, 0);
  for (const a of document.querySelectorAll('header nav a')) {
    a.toggleAttribute('aria-current', a.hash === '#' + name);
  }
  views[name].show?.();
}

function badge() {
  const o = $('#conn');
  const s = logger.status;
  if (logger.state === 'connected' && s) {
    const map = { recording: ['ok', 'recording'], power_hold: ['warn', 'power lost'], no_storage: ['bad', 'no storage'], starting: ['warn', 'starting'], offline: ['bad', 'logger offline'] };
    const [st, text] = map[s.state] ?? ['warn', s.state ?? '?'];
    o.dataset.state = st;
    o.textContent = `${text}${s.sim ? ' (sim)' : ''} · ${logger.kind === 'ble' ? 'Bluetooth' : 'Wi-Fi'}${logger.live ? ' · live' : ''}`;
  } else if (logger.state === 'connecting') {
    o.dataset.state = 'warn';
    o.textContent = 'connecting';
  } else if (logger.state === 'lost') {
    o.dataset.state = 'bad';
    o.textContent = 'connection lost';
  } else {
    o.dataset.state = 'idle';
    o.textContent = 'not connected';
  }
}

for (const ev of ['state', 'status', 'live']) logger.addEventListener(ev, badge);
logger.addEventListener('status', () => logger.status && profiles.learnLabels(logger.status.ifaces));
addEventListener('hashchange', route);

(async () => {
  store.persist();
  try {
    await profiles.loadAll();
  } catch (e) {
    toast('Could not load saved buses: ' + e.message);
    console.error(e);
  }
  for (const [name, v] of Object.entries(views)) v.init($('#view-' + name));
  route();
  badge();
})();

if ('serviceWorker' in navigator && location.protocol === 'https:') {
  navigator.serviceWorker.register('sw.js').catch(() => {});
}
