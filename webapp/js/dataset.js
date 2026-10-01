// The frames the views work on: an imported log, a session downloaded from
// the logger, or live data. Lives in the engine worker; this module tracks
// what it is and keeps a summary.

import * as engine from './engine.js';
import * as profiles from './profiles.js';
import { logger, startStream, stopStream, importSession } from './logger.js';

export const dataset = new EventTarget();
Object.assign(dataset, {
  kind: null,      // 'file' | 'session' | 'live' | null
  name: '',
  summary: null,   // engine summary (frames, buses, start_s, end_s, wall_clock, markers...)
  import: null,    // last import report (bad lines etc.)
});

const emit = () => dataset.dispatchEvent(new Event('change'));
let liveTimer = null;

async function refresh() {
  const s = await engine.call('summary');
  dataset.summary = s;
  if (s.buses.join('\n') !== profiles.ws.buses.join('\n')) await profiles.setDatasetBuses(s.buses);
  emit();
}

export async function openFile(file, progress) {
  stopLive();
  const r = await engine.callProgress('importFile', progress, file);
  dataset.kind = 'file';
  dataset.name = file.name;
  dataset.import = r;
  await refresh();
  return r;
}

export async function openSession(s, progress) {
  stopLive();
  const r = await importSession(s, progress);
  dataset.kind = 'session';
  dataset.name = `session #${s.id}${s.name ? ' ' + s.name : ''}`;
  dataset.import = r;
  await refresh();
  return r;
}

export async function startLive() {
  await startStream();
  dataset.kind = 'live';
  dataset.name = logger.kind === 'ble' ? 'live (Bluetooth, ~1 Hz snapshots)' : 'live (Wi-Fi, every frame)';
  dataset.import = null;
  clearInterval(liveTimer);
  liveTimer = setInterval(refresh, 1000);
  await refresh();
}

export function stopLive() {
  clearInterval(liveTimer);
  liveTimer = null;
  if (dataset.kind === 'live') {
    stopStream();
    dataset.name += ' (stopped)';
    emit();
  }
}

export const isLive = () => dataset.kind === 'live' && logger.live;

export async function clear() {
  stopLive();
  await engine.call('clear');
  dataset.kind = null;
  dataset.name = '';
  dataset.import = null;
  await refresh();
}

logger.addEventListener('live', () => {
  if (!logger.live && dataset.kind === 'live') stopLive();
});
