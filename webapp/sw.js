// Offline cache so the installed app opens anywhere, including the car.
// Network first (to pick up new versions when online), cache as fallback.
// Only the app's own files: requests to the logger are never touched.
// Bump VERSION when the file list changes.
const VERSION = 'canlog-v3';
const FILES = [
  './', './index.html', './app.css', './manifest.webmanifest', './icon.svg', './icon-192.png', './icon-512.png',
  './vendor/uPlot.iife.min.js', './vendor/uPlot.min.css',
  './pkg/canlog.js', './pkg/canlog_bg.wasm',
  './js/main.js', './js/ui.js', './js/store.js', './js/engine.js', './js/worker.js', './js/logger.js',
  './js/profiles.js', './js/dataset.js', './js/chart.js',
  './js/views/logger.js', './js/views/data.js', './js/views/buses.js', './js/views/analysis.js',
  './js/views/plots.js', './js/views/send.js',
];

self.addEventListener('install', e => {
  e.waitUntil(caches.open(VERSION).then(c => c.addAll(FILES)).then(() => self.skipWaiting()));
});

self.addEventListener('activate', e => {
  e.waitUntil(caches.keys()
    .then(keys => Promise.all(keys.filter(k => k !== VERSION).map(k => caches.delete(k))))
    .then(() => self.clients.claim()));
});

self.addEventListener('fetch', e => {
  const url = new URL(e.request.url);
  if (e.request.method !== 'GET' || url.origin !== location.origin) return;
  e.respondWith(
    fetch(e.request).then(r => {
      if (r.ok) {
        const copy = r.clone();
        caches.open(VERSION).then(c => c.put(e.request, copy));
      }
      return r;
    }).catch(() => caches.match(e.request, { ignoreSearch: true })),
  );
});
