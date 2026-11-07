// Offline service worker. Cache-first for the app shell: once installed,
// the receiver works in airplane mode and never re-fetches its code behind
// your back. A new version only arrives when this file's VERSION changes.

const VERSION = "courierqr-v1";
const SHELL = [
  "./",
  "index.html",
  "style.css",
  "app.js",
  "worker.js",
  "courierqr.js",
  "courierqr.wasm",
  "manifest.webmanifest",
  "icon.svg",
  "icon-192.png",
  "icon-512.png",
];

self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(VERSION).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== VERSION).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (e) => {
  if (e.request.method !== "GET" || new URL(e.request.url).origin !== location.origin) return;
  e.respondWith(caches.match(e.request, { ignoreSearch: true }).then((hit) => hit || fetch(e.request)));
});
