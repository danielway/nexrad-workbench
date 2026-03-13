// Service Worker — provides cross-origin isolation headers (COOP/COEP) to enable
// SharedArrayBuffer and intercepts all fetch requests to report network metrics
// back to the main thread for UI display.
//
// Protocol:
//   SW → Client:  { type: 'network-metric', url, status, bytes, duration, ok, error? }

// Take over immediately on install (don't wait for old SW to retire).
self.addEventListener('install', () => self.skipWaiting());

// Claim all clients on activation so the SW controls the page without a second reload.
self.addEventListener('activate', (event) => {
  event.waitUntil(self.clients.claim());
});

// Report a completed network request metric to all controlled clients.
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

// Intercept all fetch requests to:
// 1. Add COOP/COEP headers for cross-origin isolation (enables SharedArrayBuffer)
// 2. Measure and report request duration, status, and bytes transferred
self.addEventListener('fetch', (event) => {
  event.respondWith((async () => {
    const url = event.request.url;
    const startTime = performance.now();

    try {
      const response = await fetch(event.request);

      // Measure response size: prefer Content-Length to avoid buffering
      let bytes = 0;
      const contentLength = response.headers.get('Content-Length');
      if (contentLength) {
        bytes = parseInt(contentLength, 10) || 0;
      }

      // Build a new response with cross-origin isolation headers injected.
      // Using 'credentialless' for COEP instead of 'require-corp' because it
      // allows cross-origin resources (e.g. AWS S3) without requiring them to
      // send Cross-Origin-Resource-Policy headers.
      const headers = new Headers(response.headers);
      headers.set('Cross-Origin-Opener-Policy', 'same-origin');
      headers.set('Cross-Origin-Embedder-Policy', 'credentialless');

      const duration = performance.now() - startTime;
      reportMetric({ url, status: response.status, bytes, duration, ok: response.ok });

      return new Response(response.body, {
        status: response.status,
        statusText: response.statusText,
        headers,
      });
    } catch (err) {
      const duration = performance.now() - startTime;
      reportMetric({ url, status: 0, bytes: 0, duration, ok: false, error: err.message });
      throw err;
    }
  })());
});
