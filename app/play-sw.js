// Mercs & Mines: the play client's service worker (BP-3c-4).
// A root file registered with scope '/play', so it controls /play.html and /play/* and nothing else
// (a worker under /play/ could never control /play.html). It intercepts only allow-listed GETs:
// /api/*, music (Audio sends Range requests), the test-network RPC and every non-GET pass through.
// CACHE is bumped by hand by every client brick that changes app/play*; activate deletes the old one.
let CACHE = 'mm-play-BP-3c-4';
const CDN_CACHE = 'mm-cdn-1'; // pinned, immutable CDN and font URLs: kept across deploys
const SHELL = [
  '/play.html',
  '/play/play.css',
  '/play/main.js',
  '/play/api.js',
  '/play/wallet.js',
  '/play/checkout.js',
  '/play/reveal.js',
  '/play/hexmap.js',
  '/play/battle.js',
  '/assets/MM_title.png',
  '/assets/favicon-512.png',
];
SHELL.push('/play/demo_ways_in.js', '/play/demo_card.js'); CACHE += '+demo-BP-3c-4'; // demo-only (ODQ-69)
const CDN_HOSTS = ['cdn.jsdelivr.net', 'fonts.googleapis.com', 'fonts.gstatic.com'];
const NETWORK_DEADLINE_MS = 4000; // a hung venue network answers from the cache after this
const MATCH = { ignoreSearch: true }; // /play.html?debug=1 finds the /play.html entry

self.addEventListener('install', event => {
  event.waitUntil(
    caches.open(CACHE)
      .then(cache => cache.addAll(SHELL.map(u => new Request(u, { cache: 'reload' }))))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener('activate', event => {
  event.waitUntil(
    caches.keys()
      .then(keys => Promise.all(keys
        .filter(k => k.startsWith('mm-play-') && k !== CACHE)
        .map(k => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener('fetch', event => {
  const request = event.request;
  if (request.method !== 'GET') return;
  const url = new URL(request.url);
  if (url.pathname.startsWith('/api/')) return;
  if (url.pathname.startsWith('/assets/music/')) return;
  if (url.origin === self.location.origin) {
    if (url.pathname === '/play.html' || url.pathname.startsWith('/play/')) {
      event.respondWith(networkFirst(request, url));
    } else if (url.pathname.startsWith('/assets/art/') || SHELL.includes(url.pathname)) {
      event.respondWith(cacheFirst(request, CACHE));
    }
    return;
  }
  if (CDN_HOSTS.includes(url.hostname)) event.respondWith(cacheFirst(request, CDN_CACHE));
});

// The network leg revalidates (no-cache), so a deploy is never hidden by the browser's HTTP cache.
// It is never aborted: a late answer still refreshes the cache.
async function networkFirst(request, url) {
  const fresh = fetch(new Request(request, { cache: 'no-cache' })).then(response => {
    if (response.ok) {
      const copy = response.clone();
      caches.open(CACHE).then(cache => cache.put(url.origin + url.pathname, copy)).catch(() => {});
    }
    return response;
  });
  fresh.catch(() => {});
  const late = new Promise(resolve => setTimeout(resolve, NETWORK_DEADLINE_MS, 'late'));
  try {
    const first = await Promise.race([fresh, late]);
    if (first !== 'late') return first;
    const hit = await caches.match(request, MATCH);
    return hit || fresh; // no copy yet: keep waiting, a slow first visit is not failed
  } catch (err) {
    const hit = await caches.match(request, MATCH);
    if (hit) return hit;
    throw err;
  }
}

// A miss goes to the network with no deadline: the same fetch the page makes without a worker.
async function cacheFirst(request, name) {
  const hit = await caches.match(request);
  if (hit) return hit;
  const response = await fetch(request);
  if (response.ok || response.type === 'opaque') {
    const copy = response.clone();
    caches.open(name).then(cache => cache.put(request, copy)).catch(() => {});
  }
  return response;
}
