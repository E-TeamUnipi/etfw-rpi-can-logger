// Buses tab: bus profiles (name, bitrates, DBCs) and the DBC library,
// plus export/import of everything as one workspace file.

import * as profiles from '../profiles.js';
import * as store from '../store.js';
import { logger, busName } from '../logger.js';
import { h, fill, toast, ask, download } from '../ui.js';

let profSec, dbcSec, wsSec;

export function init(root) {
  profSec = h('section');
  dbcSec = h('section');
  wsSec = h('section');
  root.append(profSec, dbcSec, wsSec);
  profiles.changed.addEventListener('change', render);
  render();
}

/** One table row per profile, plus an empty row to create one. Name and
 *  bitrate save on change. */
function profileRow(p) {
  const save = async extra => {
    if (!name.value.trim()) return toast('Give the bus a name');
    await profiles.saveProfile({ ...p, ...extra, name: name.value.trim(), bitrate: Math.round(+br.value * 1000) || 0 });
  };
  const name = h('input', { type: 'text', value: p.name ?? '', placeholder: 'New bus, e.g. Powertrain', required: true, 'aria-label': 'Bus name', onchange: () => p.id && save({}) });
  const br = h('input', { type: 'number', value: p.bitrate ? p.bitrate / 1000 : 500, min: 10, max: 1000, step: 'any', 'aria-label': 'Bitrate kbit/s', onchange: () => p.id && save({}) });
  const addSel = h('select', { 'aria-label': 'Add a DBC file' }, h('option', { value: '' }, p.id ? 'Add DBC…' : 'DBC…'),
    profiles.ws.dbcs.filter(d => !(p.dbcs ?? []).includes(d.id)).map(d => h('option', { value: d.id }, profiles.dbcLabel(d.id))));
  addSel.onchange = () => addSel.value && (p.id ? save({ dbcs: [...p.dbcs, addSel.value] }) : null);
  return h('tr', {},
    h('td', {}, name),
    h('td', {}, br),
    h('td', {}, h('span', { 'data-chips': '' },
      (p.dbcs ?? []).map(id => h('mark', {}, profiles.dbcLabel(id),
        h('button', { value: 'secondary', title: 'Remove from this bus', onclick: () => save({ dbcs: p.dbcs.filter(x => x !== id) }) }, '×'))),
      profiles.ws.dbcs.length > (p.dbcs?.length ?? 0) && addSel)),
    h('td', { 'data-num': '' }, p.id
      ? h('button', { value: 'secondary', onclick: async () => {
          if (await ask(`Delete the bus profile "${p.name}"? Its DBC files stay in the library.`, 'Delete')) profiles.deleteProfile(p.id);
        } }, 'Delete')
      : h('button', { onclick: () => save({ dbcs: addSel.value ? [addSel.value] : [] }) }, 'Create')));
}

function render() {
  const live = logger.status?.ifaces ?? [];
  const missing = live.map(busName).filter(n => !profiles.ws.profiles.some(p => p.name.toLowerCase() === n.toLowerCase()));
  fill(profSec,
    h('h2', {}, h('span', {}, 'Bus profiles')),
    h('p', {}, h('small', {}, 'One profile per CAN bus. Name it like the adapter label in logger.conf (can.<serial>.label) so live data and downloaded sessions map to it automatically. A bus can use several DBC files.')),
    h('figure', {}, h('table', { 'data-form': '' },
      h('thead', {}, h('tr', {}, h('th', {}, 'Name'), h('th', {}, 'kbit/s'), h('th', {}, 'DBC files'), h('th'))),
      h('tbody', {}, profiles.ws.profiles.map(profileRow), profileRow({})))),
    missing.length > 0 && h('p', {}, h('small', {}, 'Logger buses without a profile: '),
      missing.map(n => h('button', { value: 'secondary', onclick: () => {
        const i = live.find(x => busName(x) === n);
        profiles.saveProfile({ name: n, bitrate: i?.bitrate ?? 500000 });
      } }, `+ ${n}`))));

  const file = h('input', { type: 'file', accept: '.dbc', multiple: true });
  file.onchange = async () => {
    for (const f of file.files) {
      try {
        const d = await profiles.addDbc(f);
        toast(`${f.name}: ${d.summary.messages} messages` + (d.summary.warnings.length ? `, ${d.summary.warnings.length} warnings` : ''), 4000);
      } catch (e) { toast(`${f.name}: ${e.message}`, 6000); }
    }
    file.value = '';
  };
  const used = id => profiles.ws.profiles.filter(p => p.dbcs.includes(id)).map(p => p.name);
  fill(dbcSec,
    h('h2', {}, h('span', {}, 'DBC files')),
    h('form', {}, file),
    profiles.ws.dbcs.length ? h('figure', {}, h('table', {},
      h('thead', {}, h('tr', {}, ['File', 'Version', 'Messages', 'Signals', 'Bitrate in DBC', 'Used by', ''].map(t => h('th', {}, t)))),
      h('tbody', {}, profiles.ws.dbcs.map(d => h('tr', { 'data-state': d.summary?.warnings.length ? 'warn' : '' },
        h('td', {}, d.name, d.summary?.warnings.length ? h('details', {}, h('summary', {}, `${d.summary.warnings.length} warnings`),
          h('ul', {}, d.summary.warnings.slice(0, 50).map(w => h('li', {}, h('small', {}, w))))) : null),
        h('td', {}, d.summary?.version ?? '–'),
        h('td', { 'data-num': '' }, d.summary?.messages ?? '?'), h('td', { 'data-num': '' }, d.summary?.signals ?? '?'),
        h('td', {}, d.summary?.bitrate ? `${d.summary.bitrate / 1000} kbit/s` : '–'),
        h('td', {}, used(d.id).join(', ') || '–'),
        h('td', {}, h('button', { value: 'secondary', onclick: async () => {
          if (await ask(`Delete ${d.name} from this browser?`, 'Delete')) profiles.removeDbc(d.id);
        } }, 'Delete'))))))) : h('p', {}, 'No DBC files yet. They are stored only in this browser.'));

  const imp = h('input', { type: 'file', accept: '.json' });
  imp.onchange = async () => {
    try {
      await store.importWorkspace(JSON.parse(await imp.files[0].text()));
      await profiles.loadAll();
      toast('Workspace imported');
    } catch (e) { toast('Import failed: ' + e.message, 6000); }
    imp.value = '';
  };
  fill(wsSec,
    h('h2', {}, h('span', {}, 'Backup')),
    h('p', {}, h('small', {}, 'DBCs, bus profiles and plot workbooks live only in this browser. Export them to move to another device or browser, or as a backup.')),
    h('form', {},
      h('button', { type: 'button', onclick: async () => {
        const ws = await store.exportWorkspace();
        download(`canlogger-workspace-${new Date().toISOString().slice(0, 10)}.json`, new Blob([JSON.stringify(ws)], { type: 'application/json' }));
      } }, 'Export workspace'),
      h('label', {}, 'Import: ', imp)));
}

logger.addEventListener('status', () => {
  // offer profiles for newly seen logger buses (cheap: only when the set changes)
  const key = (logger.status?.ifaces ?? []).map(busName).join('\n');
  if (key !== render.lastKey) {
    render.lastKey = key;
    if (profSec) render();
  }
});
