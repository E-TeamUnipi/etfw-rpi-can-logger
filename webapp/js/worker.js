// Web Worker hosting the WebAssembly engine (crates/canlog-wasm), so log
// imports and analysis never block the page.

import init, { Engine } from '../pkg/canlog.js';

await init();
const engine = new Engine();

const JSON_FNS = new Set(['load_dbc', 'messages', 'import_end', 'summary', 'bus_load', 'msg_stats', 'latest', 'rta']);

async function importStream(stream, name, id) {
  if (/\.gz$/i.test(name)) stream = stream.pipeThrough(new DecompressionStream('gzip'));
  engine.import_begin();
  const reader = stream.getReader();
  let bytes = 0, last = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    engine.import_push(value);
    bytes += value.length;
    if (bytes - last > 4 << 20) {
      last = bytes;
      postMessage({ id, progress: bytes });
    }
  }
  return JSON.parse(engine.import_end());
}

onmessage = async ({ data: { id, fn, args } }) => {
  try {
    let result;
    if (fn === 'importFile') {
      const [file] = args;
      result = await importStream(file.stream(), file.name, id);
    } else if (fn === 'importStream') {
      // a fetch body transferred from the page (downloads from the logger)
      const [stream, name] = args;
      result = await importStream(stream, name, id);
    } else {
      result = engine[fn](...args);
      if (JSON_FNS.has(fn)) result = JSON.parse(result);
    }
    const transfer = result instanceof Float64Array || result instanceof Uint8Array ? [result.buffer] : [];
    postMessage({ id, result }, transfer);
  } catch (e) {
    postMessage({ id, error: String(e?.message ?? e) });
  }
};

postMessage({ ready: true });
