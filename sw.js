// sw.js — Service Worker proxy
// Intercepts /nim-proxy/* → integrate.api.nvidia.com/v1/*
// Intercepts /kucoin-proxy/* → api.kucoin.com/*
// Must be served as a real file (blob: scheme is not supported for SW registration)

const NIM_BASE    = 'https://integrate.api.nvidia.com/v1';
const KUCOIN_BASE = 'https://api.kucoin.com';

self.addEventListener('install',  () => self.skipWaiting());
self.addEventListener('activate', e  => e.waitUntil(self.clients.claim()));

self.addEventListener('fetch', event => {
  const url = new URL(event.request.url);

  // ── NIM proxy ──────────────────────────────────────────────
  if (url.pathname.startsWith('/nim-proxy')) {
    const upstream = NIM_BASE + url.pathname.replace('/nim-proxy', '') + url.search;
    event.respondWith(proxyFetch(event.request, upstream));
    return;
  }

  // ── KuCoin proxy ───────────────────────────────────────────
  if (url.pathname.startsWith('/kucoin-proxy')) {
    const upstream = KUCOIN_BASE + url.pathname.replace('/kucoin-proxy', '') + url.search;
    event.respondWith(proxyFetch(event.request, upstream));
    return;
  }
});

async function proxyFetch(originalRequest, upstreamUrl) {
  try {
    const isBodyless = ['GET', 'HEAD'].includes(originalRequest.method);
    const init = {
      method:  originalRequest.method,
      headers: originalRequest.headers,
      body:    isBodyless ? undefined : originalRequest.body,
    };
    // Required for streaming request bodies (NIM SSE)
    if (!isBodyless) init.duplex = 'half';

    const response = await fetch(upstreamUrl, init);

    // Re-expose the response with permissive CORS headers so the page can read it
    const headers = new Headers(response.headers);
    headers.set('Access-Control-Allow-Origin', '*');
    headers.set('Access-Control-Allow-Headers', '*');

    return new Response(response.body, {
      status:     response.status,
      statusText: response.statusText,
      headers,
    });
  } catch (err) {
    return new Response(
      JSON.stringify({ error: err.message }),
      { status: 502, headers: { 'Content-Type': 'application/json', 'Access-Control-Allow-Origin': '*' } }
    );
  }
}
