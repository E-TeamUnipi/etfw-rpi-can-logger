// Everything the app keeps lives in this browser (IndexedDB): DBC files,
// bus profiles, the mapping of log buses to profiles, workbooks, settings.
// Nothing is uploaded anywhere.

const DB = 'canlogger';
const STORES = ['dbcs', 'profiles', 'workbooks', 'settings'];

let dbp;
function db() {
  dbp ??= new Promise((res, rej) => {
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => {
      for (const s of STORES) if (!r.result.objectStoreNames.contains(s)) r.result.createObjectStore(s, { keyPath: 'id' });
    };
    r.onsuccess = () => res(r.result);
    r.onerror = () => rej(r.error);
  });
  return dbp;
}

async function tx(store, mode, fn) {
  const d = await db();
  return new Promise((res, rej) => {
    const t = d.transaction(store, mode);
    const req = fn(t.objectStore(store));
    t.oncomplete = () => res(req?.result);
    t.onerror = () => rej(t.error);
  });
}

export const all = store => tx(store, 'readonly', s => s.getAll());
export const get = (store, id) => tx(store, 'readonly', s => s.get(id));
export const put = (store, obj) => tx(store, 'readwrite', s => s.put(obj));
export const del = (store, id) => tx(store, 'readwrite', s => s.delete(id));

export async function setting(key, fallback) {
  return (await get('settings', key))?.value ?? fallback;
}
export const setSetting = (key, value) => put('settings', { id: key, value });

export const newId = () => crypto.randomUUID();

/** Ask the browser not to evict our data (Safari drops site data after 7 days otherwise). */
export function persist() {
  navigator.storage?.persist?.().catch(() => {});
}

// ---- workspace export/import (one JSON file with DBCs as base64)

const b64 = buf => {
  let s = '';
  const b = new Uint8Array(buf);
  for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
  return btoa(s);
};
const unb64 = s => Uint8Array.from(atob(s), c => c.charCodeAt(0)).buffer;

export async function exportWorkspace() {
  const dbcs = (await all('dbcs')).map(d => ({ ...d, bytes: b64(d.bytes) }));
  return {
    format: 'canlogger-workspace', version: 1, exported: new Date().toISOString(),
    dbcs, profiles: await all('profiles'), workbooks: await all('workbooks'),
    settings: (await all('settings')).filter(s => ['busmap', 'rta_overrides'].includes(s.id)),
  };
}

export async function importWorkspace(ws) {
  if (ws?.format !== 'canlogger-workspace') throw new Error('not a workspace file');
  for (const d of ws.dbcs ?? []) await put('dbcs', { ...d, bytes: unb64(d.bytes) });
  for (const p of ws.profiles ?? []) await put('profiles', p);
  for (const w of ws.workbooks ?? []) await put('workbooks', w);
  for (const s of ws.settings ?? []) await put('settings', s);
}
