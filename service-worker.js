// Service Worker — three responsibilities:
//   1. Inject COOP/COEP response headers to enable cross-origin isolation
//      (required for SharedArrayBuffer in the worker / WASM).
//   2. Cache the app shell so an installed PWA (Add to Home Screen on iOS)
//      can launch without a network connection.
//   3. Report per-request network metrics back to the UI.
//
// Protocol:
//   SW → Client:  { type: 'network-metric', url, status, bytes, duration, ok, error? }
//
// Bump CACHE_VERSION whenever the shape of cached content changes in a way
// that requires old entries to be evicted. Byte-level changes to this file
// already cause the browser to install a fresh SW, so the activate handler's
// cleanup of stale caches is what actually runs the eviction.
const CACHE_VERSION = 'v1';
const CACHE_NAME = `nexrad-workbench-${CACHE_VERSION}`;

// App shell — static, non-hashed files we can name ahead of time.
// Trunk-emitted hashed assets (index-{hash}.js, *_bg.wasm, snippets/*) are
// runtime-cached on first fetch below rather than listed here.
const PRECACHE_URLS = [
  './',
  './index.html',
  './worker.js',
  './assets/manifest.webmanifest',
  './assets/favicon.svg',
  './assets/apple-touch-icon.png',
  './assets/icon-192.png',
  './assets/icon-512.png',
];

self.addEventListener('install', (event) => {
  event.waitUntil((async () => {
    const cache = await caches.open(CACHE_NAME);
    // Add entries individually so one missing asset doesn't abort the whole
    // install — the fetch handler will still work without the cache.
    await Promise.all(
      PRECACHE_URLS.map((url) => cache.add(url).catch(() => {}))
    );
    await self.skipWaiting();
  })());
});

self.addEventListener('activate', (event) => {
  event.waitUntil((async () => {
    const keys = await caches.keys();
    await Promise.all(
      keys
        .filter((k) => k.startsWith('nexrad-workbench-') && k !== CACHE_NAME)
        .map((k) => caches.delete(k))
    );
    await self.clients.claim();
  })());
});

// Clone a response with COOP/COEP headers injected. 'credentialless' is used
// for COEP instead of 'require-corp' so cross-origin resources (AWS S3, NOAA)
// don't need to send Cross-Origin-Resource-Policy headers of their own.
function withIsolationHeaders(response) {
  const headers = new Headers(response.headers);
  headers.set('Cross-Origin-Opener-Policy', 'same-origin');
  headers.set('Cross-Origin-Embedder-Policy', 'credentialless');
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
}

async function reportMetric(metric) {
  const clients = await self.clients.matchAll();
  for (const client of clients) {
    client.postMessage({
      type: 'network-metric',
      url: metric.url,
      status: metric.status,
      bytes: metric.bytes,
      duration: metric.duration,
      ok: metric.ok,
      error: metric.error || null,
    });
  }
}

async function fetchAndReport(request) {
  const url = request.url;
  const startTime = performance.now();
  try {
    const response = await fetch(request);
    const duration = performance.now() - startTime;
    const contentLength = response.headers.get('Content-Length');
    const bytes = contentLength ? parseInt(contentLength, 10) || 0 : 0;
    reportMetric({ url, status: response.status, bytes, duration, ok: response.ok });
    return response;
  } catch (err) {
    const duration = performance.now() - startTime;
    reportMetric({ url, status: 0, bytes: 0, duration, ok: false, error: err.message });
    throw err;
  }
}

function isSameOrigin(request) {
  try {
    return new URL(request.url).origin === self.location.origin;
  } catch {
    return false;
  }
}

async function handleFetch(request) {
  // Navigation (HTML document) requests: network-first so users see new
  // deploys immediately, falling back to the cached shell when offline.
  if (request.mode === 'navigate') {
    try {
      const response = await fetchAndReport(request);
      if (response.ok && isSameOrigin(request)) {
        const cache = await caches.open(CACHE_NAME);
        cache.put(request, response.clone()).catch(() => {});
      }
      return withIsolationHeaders(response);
    } catch (err) {
      const cached =
        (await caches.match(request)) ||
        (await caches.match('./index.html')) ||
        (await caches.match('./'));
      if (cached) return withIsolationHeaders(cached);
      throw err;
    }
  }

  // Same-origin assets (WASM, JS, worker.js, icons, manifest): cache-first,
  // runtime-cache on miss. Hashed filenames from Trunk make this safe — a
  // new build produces new filenames, so the cache never serves stale code.
  if (isSameOrigin(request) && request.method === 'GET') {
    const cached = await caches.match(request);
    if (cached) return withIsolationHeaders(cached);
    const response = await fetchAndReport(request);
    if (response.ok) {
      const cache = await caches.open(CACHE_NAME);
      cache.put(request, response.clone()).catch(() => {});
    }
    return withIsolationHeaders(response);
  }

  // Cross-origin (AWS S3 radar archives, NOAA): never cache — these are
  // large and already stored in IndexedDB. Just pass through with headers
  // and metrics.
  const response = await fetchAndReport(request);
  return withIsolationHeaders(response);
}

self.addEventListener('fetch', (event) => {
  event.respondWith(handleFetch(event.request));
});
