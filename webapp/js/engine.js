// Promise wrapper around the engine worker: `await engine.call('latest', bus)`.

const worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
const pending = new Map();
let next = 1;
let readyResolve;
const ready = new Promise(r => (readyResolve = r));

worker.onmessage = ({ data }) => {
  if (data.ready) return readyResolve();
  const p = pending.get(data.id);
  if (!p) return;
  if ('progress' in data) return p.progress?.(data.progress);
  pending.delete(data.id);
  data.error ? p.reject(new Error(data.error)) : p.resolve(data.result);
};
worker.onerror = e => console.error('engine worker', e);

const transferables = args => args.filter(a => a instanceof ArrayBuffer || a instanceof ReadableStream);

export async function call(fn, ...args) {
  await ready;
  const id = next++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    worker.postMessage({ id, fn, args }, transferables(args));
  });
}

/** Like call, with a progress callback (bytes read) for imports. */
export async function callProgress(fn, progress, ...args) {
  await ready;
  const id = next++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject, progress });
    worker.postMessage({ id, fn, args }, transferables(args));
  });
}
