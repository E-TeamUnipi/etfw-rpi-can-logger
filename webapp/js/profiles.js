// Bus profiles: a named CAN bus (e.g. "Powertrain") with its bitrates and
// one or more DBC files. Buses found in a dataset (live interfaces by label,
// or the interface/channel names of an imported log) map to a profile; the
// engine then decodes that bus with the profile's DBCs.

import * as engine from './engine.js';
import * as store from './store.js';
import { logger, busName } from './logger.js';

// Interface name -> adapter label, learned from the logger's status and
// kept, so logs exported by the logger (which name buses can0, can1...)
// map to the label-named profiles even offline.
let ifaceLabels = {};

export const ws = {
  dbcs: [],          // {id, name, bytes, added, summary}
  profiles: [],      // {id, name, bitrate, dbcs: [dbcId]}
  busmap: {},        // dataset bus name -> profile id ('' = none)
  buses: [],         // bus names in the current dataset
};

export const changed = new EventTarget();
const emit = () => changed.dispatchEvent(new Event('change'));

export async function loadAll() {
  ws.dbcs = await store.all('dbcs');
  ws.profiles = (await store.all('profiles')).sort((a, b) => a.name.localeCompare(b.name));
  ws.busmap = await store.setting('busmap', {});
  ifaceLabels = await store.setting('iface_labels', {});
  for (const d of ws.dbcs) d.summary = await engine.call('load_dbc', d.id, new Uint8Array(d.bytes));
  await sync();
}

/** Push profile/bus -> DBC assignments to the engine. */
export async function sync() {
  for (const p of ws.profiles) await engine.call('assign', '@' + p.id, JSON.stringify(p.dbcs));
  for (const b of ws.buses) {
    const p = profileForBus(b);
    await engine.call('assign', b, JSON.stringify(p?.dbcs ?? []));
  }
  emit();
}

export function setDatasetBuses(buses) {
  ws.buses = buses;
  return sync();
}

/** Profile for a dataset bus: explicit mapping, else same name as the bus
 *  or as the adapter label of that interface (case-insensitive). */
export function profileForBus(bus) {
  if (bus in ws.busmap) return ws.profiles.find(p => p.id === ws.busmap[bus]) ?? null;
  const names = [bus, ifaceLabels[bus]].filter(Boolean).map(n => n.toLowerCase());
  return ws.profiles.find(p => names.includes(p.name.toLowerCase())) ?? null;
}

/** Remember interface labels reported by the logger. */
export async function learnLabels(ifaces) {
  let changed = false;
  for (const i of ifaces) {
    if (i.label && ifaceLabels[i.name] !== i.label) {
      ifaceLabels = { ...ifaceLabels, [i.name]: i.label };
      changed = true;
    }
  }
  if (changed) {
    await store.setSetting('iface_labels', ifaceLabels);
    await sync();
  }
}

/** Dataset bus currently decoded with this profile, if any. */
export const busForProfile = pid => ws.buses.find(b => profileForBus(b)?.id === pid) ?? null;

/** Engine bus key for a profile: its dataset bus, or a data-less alias. */
export const engineBus = pid => busForProfile(pid) ?? '@' + pid;

export async function mapBus(bus, pid) {
  ws.busmap = { ...ws.busmap, [bus]: pid };
  await store.setSetting('busmap', ws.busmap);
  await sync();
}

/** CAN FD data-phase bitrate of a profile, taken from its DBCs (0 = none). */
export const dataBitrate = p => p?.dbitrate || p?.dbcs.map(id => ws.dbcs.find(d => d.id === id)?.summary?.data_bitrate).find(Boolean) || 0;

/** {bus: [bitrate, dbitrate]} for the dataset buses. */
export function bitrates() {
  const out = {};
  for (const b of ws.buses) {
    const p = profileForBus(b);
    const live = logger.status?.ifaces.find(i => busName(i) === b);
    const dbcRate = p?.dbcs.map(id => ws.dbcs.find(d => d.id === id)?.summary).find(s => s?.bitrate);
    out[b] = [p?.bitrate || live?.bitrate || dbcRate?.bitrate || 0, dataBitrate(p) || live?.dbitrate || 0];
  }
  return out;
}

export async function addDbc(file) {
  const bytes = await file.arrayBuffer();
  const d = { id: store.newId(), name: file.name, bytes, added: Date.now() };
  d.summary = await engine.call('load_dbc', d.id, new Uint8Array(bytes));
  await store.put('dbcs', { id: d.id, name: d.name, bytes, added: d.added });
  ws.dbcs.push(d);
  emit();
  return d;
}

export async function removeDbc(id) {
  await store.del('dbcs', id);
  await engine.call('remove_dbc', id);
  ws.dbcs = ws.dbcs.filter(d => d.id !== id);
  for (const p of ws.profiles.filter(p => p.dbcs.includes(id))) {
    p.dbcs = p.dbcs.filter(x => x !== id);
    await store.put('profiles', p);
  }
  await sync();
}

export async function saveProfile(p) {
  p.id ??= store.newId();
  p.dbcs ??= [];
  await store.put('profiles', p);
  ws.profiles = (await store.all('profiles')).sort((a, b) => a.name.localeCompare(b.name));
  await sync();
  return p;
}

export async function deleteProfile(id) {
  await store.del('profiles', id);
  ws.profiles = ws.profiles.filter(p => p.id !== id);
  await sync();
}

/** Messages (with signals) known for a profile. */
export const messages = pid => engine.call('messages', engineBus(pid));

/** DBC file name with its VERSION, e.g. "can_v.dbc 1.0". */
export function dbcLabel(id) {
  const d = ws.dbcs.find(d => d.id === id);
  return d ? d.name + (d.summary?.version ? ' ' + d.summary.version : '') : '(missing)';
}

export const profileName = pid => ws.profiles.find(p => p.id === pid)?.name ?? '?';
