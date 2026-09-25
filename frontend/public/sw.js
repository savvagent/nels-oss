// App-shell service worker for the Nels PWA.
//
// Strategy:
//   - Navigations / HTML  → network-first (fall back to the cached shell when
//     offline). This is what lets a new deploy reach already-installed PWAs:
//     the freshest index.html is fetched while online, so it always references
//     the current content-hashed bundle instead of one frozen at install time.
//   - Hashed build assets  → cache-first. Vite content-hashes /assets/* names,
//     so each URL is immutable and safe to serve from cache forever; a new
//     build produces new names that are fetched on demand.
//   - /api/*               → never intercepted; always hits the network live.
//
// CACHE_NAME ends in a build-hash placeholder that the Vite build stamps with
// the short commit SHA (see stampServiceWorker in vite.config.js), so the
// emitted dist/sw.js byte-changes every release. That is what makes the browser
// treat each deploy as a new worker, evict the prior cache in `activate`, and
// surface the in-app "Update available" prompt — without anyone hand-bumping a
// version. Do not hardcode a literal cache name here; keep the placeholder.
const CACHE_NAME = 'nels-budget-rag-cache-__BUILD_HASH__';

// Minimal app shell. Deliberately excludes the hashed /assets/* bundle (its
// name changes every build; it is cached at runtime) and the dev-only /src/*
// paths a prior version precached by mistake. The icon entries carry the same
// ?rev=2 cache-busting query the page and manifest reference, so this precache
// matches what a fresh install actually requests (a plain no-query URL would be
// dead weight). Bump ?rev in lockstep across index.html / manifest.json / here.
const SHELL = [
  '/',
  '/index.html',
  '/manifest.json',
  '/icon.svg?rev=2',
  '/favicon.ico?rev=2',
  '/favicon-192x192.png?rev=2',
  '/favicon-512x512.png?rev=2',
  '/apple-touch-icon.png?rev=2',
];

// Install: precache the shell. A new worker intentionally parks in `waiting`
// (no skipWaiting here) so the app can surface an "update available" prompt;
// the page sends SKIP_WAITING when the user opts in (see the message handler).
self.addEventListener('install', (e) => {
  e.waitUntil(caches.open(CACHE_NAME).then((cache) => cache.addAll(SHELL)));
});

// Let the page promote a waiting worker on demand (the in-app update button).
self.addEventListener('message', (e) => {
  if (e.data && e.data.type === 'SKIP_WAITING') self.skipWaiting();
});

// Activate: drop every cache that isn't the current version, then claim open
// clients so the fixed worker controls them without requiring a second reload.
self.addEventListener('activate', (e) => {
  e.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((key) => key !== CACHE_NAME).map((key) => caches.delete(key)))
      )
      .then(() => self.clients.claim())
  );
});

self.addEventListener('fetch', (e) => {
  const { request } = e;
  const url = new URL(request.url);

  // Only handle same-origin GETs; let API calls, cross-origin, and non-GET
  // requests go straight to the network untouched.
  if (request.method !== 'GET' || url.origin !== self.location.origin) return;
  if (url.pathname.startsWith('/api/')) return;

  // Never intercept Vite dev-server traffic. This worker only ships in prod,
  // but because it calls clients.claim() a copy installed during a prior
  // `vite build`/`preview` run keeps controlling the SAME localhost origin
  // after you switch to `vite dev`. The cache-first branch below would then
  // freeze Vite's versioned dev modules (e.g. runtime-*.js?v=<hash>), so a
  // later dep re-optimization leaves the page running two module passes at
  // once — the "Cannot read properties of undefined (reading 'call')" /
  // duplicate-Svelte-runtime crash — while the stale HMR client breaks the
  // WebSocket.
  //
  // Prod-safety assumption: production serves no asset under these path
  // prefixes (the build emits content-hashed /assets/* filenames, not /src/ or
  // /node_modules/) and adds no `?v=`/`?t=` query to any cacheable URL, so this
  // bypass is a no-op in production. If a future production route ever relies on
  // a `?v=`/`?t=` query for caching, narrow those two checks to the dev paths.
  if (
    url.pathname.startsWith('/@vite/') ||
    url.pathname.startsWith('/@id/') ||
    url.pathname.startsWith('/@fs/') ||
    url.pathname.startsWith('/src/') ||
    url.pathname.startsWith('/node_modules/') ||
    url.searchParams.has('v') ||
    url.searchParams.has('t')
  ) {
    return;
  }

  // Network-first for navigations so a new deploy's index.html — and thus the
  // current hashed bundle it references — is picked up whenever online.
  if (request.mode === 'navigate') {
    e.respondWith(
      fetch(request)
        .then((networkResponse) => {
          // Only cache real 2xx documents — never a transient 500/redirect,
          // which would otherwise poison the offline shell.
          if (networkResponse.ok) {
            const copy = networkResponse.clone();
            caches.open(CACHE_NAME).then((cache) => cache.put('/index.html', copy));
          }
          return networkResponse;
        })
        .catch(() =>
          caches.match(request).then((cached) => cached || caches.match('/index.html'))
        )
    );
    return;
  }

  // Cache-first for everything else (content-hashed assets, icons, manifest).
  e.respondWith(
    caches.match(request).then(
      (cached) =>
        cached ||
        fetch(request).then((networkResponse) => {
          if (networkResponse.status === 200) {
            const copy = networkResponse.clone();
            caches.open(CACHE_NAME).then((cache) => cache.put(request, copy));
          }
          return networkResponse;
        })
    )
  );
});
