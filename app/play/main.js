// Seeker client entry point (BP-3a-1, portrait-first since BP-3a-1b / ODQ-58): title → MWA
// sign-in → join → Commander reveal → hub (the sector map since BP-3a-2). One upright layout;
// nothing is locked or blocked.
//
// Boot and reload rule (spec check §8):
//   1. no stored session → title;
//   1a. otherwise the working screen, so the stage is never blank while boot waits;
//   2. otherwise one GET /api/auth/session (not rate-limited); a 401 → title, then sign-in;
//   3. a stored campaign → its cached roster → reveal, with no limited request; the roster is
//      fetched once only if its cache is missing;
//   4. no stored campaign → one join, then one roster fetch.
// A roster or sector 409 (or 404/403: the stored campaign is stale) clears the campaign and
// joins once more; a second one in a row is the error screen. A sector 500 offers a retry of
// the sector only, never a re-join. The sector is fetched once per campaign and cached for the
// tab. Nothing runs on a timer and nothing retries by itself: every retry is a tap.

import { ApiError, store, counters, getSession, postJoin, getRoster, getSector } from './api.js';
import { loadWallet, signIn, WalletError } from './wallet.js';
import { renderReveal } from './reveal.js';
import { HexMap, PALETTE, TERRAIN_ORDER, OVERLAY } from './hexmap.js';
import { createBattle } from './battle.js';

// Booth copy, verbatim from the BP-3a-1, BP-3a-1b, BP-3a-2 and BP-3b-2 prompts (the battle's own
// copy lives in battle.js).
const COPY = {
  start: 'Report for duty',
  tagline: 'Expendability is a career path.',
  bootWorking: 'Checking your credentials...',
  signinButton: 'Sign in with your Seeker',
  signinExplainer: 'One fingerprint signs you in. Your wallet will say "authorize". This only proves the Seeker is yours: nothing is spent, nothing moves.',
  signinLna: 'Chrome may ask to let this page reach "other apps and services" on your phone. Tap Allow: that\'s how it talks to your wallet.',
  signinWallet: "Your wallet didn't finish signing you in. Tap to try again.",
  signinExpired: 'The challenge expired while you were deciding. Tap to try again.',
  signinFailed: "The signature didn't check out. Tap to try again.",
  signinCapacity: 'Sign-in is at capacity. Tap to try again in a moment.',
  sessionExpired: 'Your session has expired. Please sign in again.',
  joinWorking: 'Requisitioning your outfit...',
  joinFull: 'The demo is at capacity. Every Commander is spoken for. Please try again shortly.',
  joinClosed: 'Recruitment is closed for now.',
  rateLimited: 'Command is fielding too many requests. Try again in {n} s.',
  offline: 'Lost contact with Command. Tap to retry.',
  generic: 'Something went wrong on our side. Tap to retry.',
  revealHeading: 'Meet your Commander',
  revealOutfit: 'Your outfit',
  hubTitle: 'Your Sector',
  hubBack: 'Back',
  hubRecentre: 'Recentre',
  hubLegend: 'Legend',
  hubMissions: 'Missions',
  hubLoading: 'Surveying your sector...',
  legendVein: 'Magma vein (bigger is richer)',
  legendAnchor: 'Tectonic anchor',
  legendGateway: 'Your gateway',
  // Canon terrain names (Terrain_and_Environmental_Hazards §1), keyed by `HexTerrain`.
  terrain: {
    AshenPlains: 'Ashen Plains', ScorchedDesert: 'Scorched Desert', RuinedCity: 'Ruined City',
    CityState: 'City-State', GunOutpost: 'G.U.N. Outpost', MagmaFlow: 'Magma Flow',
    CooledMagma: 'Cooled Magma', VolcanicCaldera: 'Volcanic Caldera', Mountain: 'Mountain',
    Ravine: 'Ravine', ToxicZone: 'Toxic Zone',
  },
};

const $ = id => document.getElementById(id);
const SCREENS = ['screen-title', 'screen-signin', 'screen-working', 'screen-reveal', 'screen-hub', 'screen-error',
  'screen-missions', 'screen-briefing', 'screen-battle', 'screen-aar', 'screen-end'];
const DEBUG = new URLSearchParams(location.search).get('debug') === '1';

let busy = false;           // one wallet or server flow at a time
let pendingNotice = '';     // shown on the sign-in screen when it next opens
let errorRetry = null;      // what the error screen's button runs
let lastRoster = null;      // the last roster, kept in memory too (Reviewer BP-2b-2 F2)
let mwaState = 'loading';
let lastError = '';
let map = null;             // the sector map, kept while the campaign stays the same
let mapCampaign = null;     // the campaign `map` belongs to
let mapDrawMs = null;       // parsed response → the first drawImage, for the debug footer
let debugExtra = '';        // the last mission's `stress=… xp=…`, for the debug footer
let battle = null;          // battle.js (BP-3b-2), created at the bottom

// ── screens ─────────────────────────────────────────────────────────────────
function show(id) {
  for (const s of SCREENS) $(s).hidden = s !== id;
  renderDebug();
}

function showError(text, retry) {
  $('error-text').textContent = text;
  errorRetry = retry;
  show('screen-error');
}

function rateText(seconds) {
  return COPY.rateLimited.replace('{n}', String(seconds));
}

function note(e) {
  if (e instanceof ApiError) lastError = `${e.status} ${e.kind}: ${e.message}`;
  else if (e instanceof WalletError) lastError = `WalletError ${e.code}: ${e.message}`; // the MWA code, footer only
  else lastError = `${e && e.name}: ${e && e.message}`;
  renderDebug();
}

// The shared mapping for every session-taking call.
function handleError(e, retry) {
  note(e);
  if (e instanceof ApiError) {
    if (e.kind === 'unauth') {
      store.clearAll();
      return toSignIn(COPY.sessionExpired);
    }
    if (e.kind === 'rate') return showError(rateText(e.retryAfter), retry);
    if (e.kind === 'offline') return showError(COPY.offline, retry);
  }
  return showError(COPY.generic, retry);
}

// ── title ───────────────────────────────────────────────────────────────────
// Nothing is fetched before the first tap (Reviewer BP-3a-1 F6): no src, no preload.
const MUSIC_SRC = 'assets/music/pre_battle.mp3';
const music = new Audio();
music.preload = 'none';
music.loop = true;
music.volume = 0.5;

// The tap never waits: the music starts when it has buffered, and sign-in shows at once.
function onStart() {
  if (!music.src) music.src = MUSIC_SRC;
  music.play().catch(() => { /* audio blocked or failed: play on without it */ });
  toSignIn(pendingNotice);
}

// ── sign-in ─────────────────────────────────────────────────────────────────
// Chrome's Local Network Access prompt ("other apps and services") is resolved per site, and
// Chrome 155 may answer under either name. The line stays unless one of them says granted.
async function lnaGranted() {
  for (const name of ['loopback-network', 'local-network-access']) {
    try {
      const status = await navigator.permissions.query({ name });
      return status.state === 'granted';
    } catch { /* this name is unknown here: try the next */ }
  }
  return false;
}

function toSignIn(notice) {
  pendingNotice = '';
  $('signin-status').textContent = notice || '';
  $('signin-lna').hidden = false;
  lnaGranted().then(granted => { if (granted) $('signin-lna').hidden = true; });
  show('screen-signin');
}

function signInCopy(e) {
  if (e instanceof ApiError) {
    if (e.kind === 'bad' && /expired/i.test(e.message)) return COPY.signinExpired;
    if (e.kind === 'bad') return COPY.signinFailed;
    if (e.kind === 'unavailable') return COPY.signinCapacity;
    if (e.kind === 'rate') return rateText(e.retryAfter);
    if (e.kind === 'offline') return COPY.offline;
    return COPY.generic;
  }
  // Not found, timed out, closed, declined (-1) or not signed (-3): one line; the code is in the footer.
  if (e instanceof WalletError) return COPY.signinWallet;
  return COPY.offline; // the wallet library itself failed to load
}

async function onSignIn() {
  if (busy) return;
  // Airplane mode: say so now, rather than open a wallet whose challenge can't be fetched.
  if (navigator.onLine === false) {
    lastError = 'navigator.onLine is false';
    renderDebug();
    $('signin-status').textContent = COPY.offline;
    return;
  }
  busy = true;
  $('btn-signin').disabled = true;
  $('signin-status').textContent = '';
  try {
    const token = await signIn();
    store.setSession(token);
    store.clearCampaign(); // a fresh sign-in may be another wallet
    lastRoster = null;
    await enterCampaign(0);
  } catch (e) {
    note(e);
    $('signin-status').textContent = signInCopy(e);
  } finally {
    busy = false;
    $('btn-signin').disabled = false;
  }
}

// ── campaign ────────────────────────────────────────────────────────────────
async function enterCampaign(conflicts) {
  const camp = store.campaign();
  if (!camp) return doJoin(conflicts);
  const cached = store.roster(camp.campaign_id);
  if (cached) return showReveal(camp, cached);
  return loadRoster(camp, conflicts);
}

async function doJoin(conflicts) {
  $('working-text').textContent = COPY.joinWorking;
  show('screen-working');
  let ids;
  try {
    ids = await postJoin();
  } catch (e) {
    if (e instanceof ApiError && e.kind === 'unavailable') {
      note(e);
      const text = e.message === 'the demo is full' ? COPY.joinFull
        : e.message === 'campaign joins are closed' ? COPY.joinClosed
        : COPY.generic;
      return showError(text, () => doJoin(conflicts));
    }
    return handleError(e, () => doJoin(conflicts));
  }
  store.setCampaign(ids);
  return loadRoster({ campaign_id: ids.campaign_id, commander_id: ids.commander_id }, conflicts);
}

async function loadRoster(camp, conflicts) {
  $('working-text').textContent = COPY.joinWorking;
  show('screen-working');
  let roster;
  try {
    roster = await getRoster(camp.campaign_id);
  } catch (e) {
    if (e instanceof ApiError && (e.kind === 'conflict' || e.kind === 'notfound' || e.kind === 'forbidden')) {
      note(e);
      store.clearCampaign(); // the stored campaign is over or gone
      lastRoster = null;
      if (conflicts >= 1) return showError(COPY.generic, () => enterCampaign(0));
      return doJoin(conflicts + 1);
    }
    return handleError(e, () => loadRoster(camp, conflicts));
  }
  store.setRoster(camp.campaign_id, roster);
  return showReveal(camp, roster);
}

// The reveal never shows a dead Commander: a KIA or Shattered stored id gives way to the first
// usable one in the roster (a re-join answers with the starter, alive or not), and with none
// usable the run is over.
const isUsable = c => !c.is_kia && !c.is_shattered;

function showReveal(camp, roster) {
  lastRoster = roster;
  const commanders = roster.commanders || [];
  const stored = commanders.find(c => c.commander_id === camp.commander_id);
  if (stored && !isUsable(stored)) {
    const next = commanders.find(isUsable);
    if (!next) return battle.showEndOfRun(stored.commander_id);
    camp = { campaign_id: camp.campaign_id, commander_id: next.commander_id };
    store.setCampaign(camp);
  }
  const ok = renderReveal($('reveal-content'), roster, camp.commander_id,
    { heading: COPY.revealHeading, outfit: COPY.revealOutfit });
  if (!ok) {
    lastError = `commander ${camp.commander_id} not in roster`;
    return showError(COPY.generic, () => loadRoster(camp, 1));
  }
  show('screen-reveal');
}

// ── hub: the sector map (BP-3a-2) ───────────────────────────────────────────
// From the reveal's Continue. The same campaign's map is kept and only re-measured; a cached
// body draws with no fetch; otherwise one fetch.
async function enterHub() {
  const camp = store.campaign();
  if (!camp) return enterCampaign(0);
  if (map && mapCampaign === camp.campaign_id) {
    show('screen-hub');
    map.resize();
    return;
  }
  const cached = store.sector(camp.campaign_id);
  if (cached) return showMap(camp, cached, performance.now());
  return loadSector(camp, 0);
}

async function loadSector(camp, conflicts) {
  $('working-text').textContent = COPY.hubLoading;
  show('screen-working');
  let sector;
  try {
    sector = await getSector(camp.campaign_id);
  } catch (e) {
    if (e instanceof ApiError && (e.kind === 'conflict' || e.kind === 'notfound' || e.kind === 'forbidden')) {
      note(e);
      store.clearCampaign(); // the stored campaign is over or gone: the roster's rule
      lastRoster = null;
      if (conflicts >= 1) return showError(COPY.generic, () => enterCampaign(0));
      return doJoin(conflicts + 1);
    }
    // A 500 (after the one retry), offline, 429 or 401: the button retries the sector only.
    return handleError(e, () => loadSector(camp, conflicts));
  }
  const t0 = performance.now();
  showMap(camp, sector, t0);
  store.setSector(camp.campaign_id, sector); // after the draw, so the timing is the draw's
}

// The hub must be visible first: the canvas needs its layout size for the first draw.
function showMap(camp, sector, t0) {
  show('screen-hub');
  if (map) map.destroy();
  map = new HexMap($('hub-map'), sector);
  mapCampaign = camp.campaign_id;
  mapDrawMs = Math.round(performance.now() - t0);
  if (map.unknownTerrain.size) lastError = `unknown terrain: ${[...map.unknownTerrain].join(', ')}`;
  renderDebug();
}

function toggleLegend() {
  const legend = $('legend');
  legend.hidden = !legend.hidden;
  $('btn-legend').setAttribute('aria-expanded', String(!legend.hidden));
}

// The legend: Wiki's palette swatches with the canon names, then the overlay marks.
function buildLegend() {
  const list = $('legend-list');
  // kind: 'fill' (a filled square or dot) or 'ring' (an outline only)
  const row = (shape, kind, colour, text, wide) => {
    const li = document.createElement('li');
    li.className = wide ? 'legend-item legend-wide' : 'legend-item';
    const swatch = document.createElement('span');
    swatch.className = `swatch swatch-${shape}`;
    if (kind === 'fill') swatch.style.background = colour;
    else swatch.style.borderColor = colour;
    const label = document.createElement('span');
    label.textContent = text;
    li.append(swatch, label);
    return li;
  };
  for (const t of TERRAIN_ORDER) list.append(row('hex', 'fill', PALETTE[t], COPY.terrain[t], false));
  list.append(
    row('dot', 'fill', OVERLAY.vein, COPY.legendVein, true),
    row('dot', 'ring', OVERLAY.anchor, COPY.legendAnchor, true),
    row('hex', 'ring', OVERLAY.gateway, COPY.legendGateway, true),
  );
}

// ── boot ────────────────────────────────────────────────────────────────────
async function boot() {
  if (!store.session()) return show('screen-title');
  $('working-text').textContent = COPY.bootWorking;
  show('screen-working'); // never a blank stage while the session check is in flight
  try {
    await getSession();
  } catch (e) {
    note(e);
    if (e instanceof ApiError && e.kind === 'unauth') {
      store.clearAll();
      pendingNotice = COPY.sessionExpired;
      return show('screen-title');
    }
    return handleError(e, boot);
  }
  return enterCampaign(0);
}

// ── debug footer (?debug=1) ─────────────────────────────────────────────────
function renderDebug() {
  if (!DEBUG) return;
  $('debug').textContent =
    `mwa ${mwaState} · roster fetches ${counters.roster} · sector fetches ${counters.sector} · joins ${counters.join}`
    + ` · missions ${counters.missions} · units ${counters.units} · mission posts ${counters.mission}`
    + (mapDrawMs !== null ? ` · map draw ${mapDrawMs} ms` : '')
    + (debugExtra ? ` · ${debugExtra}` : '')
    + (lastError ? ` · last error: ${lastError}` : '');
}

// ── wiring ──────────────────────────────────────────────────────────────────
function fillCopy() {
  $('btn-start').textContent = COPY.start;
  $('title-tagline').textContent = COPY.tagline;
  $('btn-signin').textContent = COPY.signinButton;
  $('signin-explainer').textContent = COPY.signinExplainer;
  $('signin-lna').textContent = COPY.signinLna;
  $('hub-title').textContent = COPY.hubTitle;
  $('btn-back').textContent = COPY.hubBack;
  $('btn-recentre').textContent = COPY.hubRecentre;
  $('btn-legend').textContent = COPY.hubLegend;
  $('btn-missions').textContent = COPY.hubMissions;
}

function wire() {
  $('btn-start').addEventListener('click', onStart);
  $('btn-signin').addEventListener('click', onSignIn);
  $('btn-error-retry').addEventListener('click', () => { const r = errorRetry; errorRetry = null; if (r) r(); });
  $('btn-continue').addEventListener('click', enterHub);
  $('btn-back').addEventListener('click', () => {
    const camp = store.campaign();
    if (camp && lastRoster) return showReveal(camp, lastRoster); // the map instance is kept
    return enterCampaign(0);
  });
  $('btn-recentre').addEventListener('click', () => { if (map) map.home(); });
  $('btn-legend').addEventListener('click', toggleLegend);
  $('btn-missions').addEventListener('click', () => battle.openMissions());
  // The toolbar showing or hiding, or a turn of the phone, resizes the stage.
  window.addEventListener('resize', () => { if (map && !$('screen-hub').hidden) map.resize(); });
  if (DEBUG) $('debug').hidden = false;
}

// ── battle (BP-3b-2) ────────────────────────────────────────────────────────
function dropMap() {
  if (map) map.destroy();
  map = null;
  mapCampaign = null;
}

// After a lost mission: one roster fetch, which lands on the reveal with the server's numbers.
function reloadRoster() {
  const camp = store.campaign();
  if (!camp) return enterCampaign(0);
  return loadRoster(camp, 0);
}

// The stored campaign is over or not this wallet's: the roster's rule (clear, re-join once).
function staleCampaign() {
  store.clearCampaign();
  lastRoster = null;
  dropMap();
  return doJoin(1);
}

// New run: the old campaign is retired, so a fresh join starts the next one.
function startFresh() {
  store.clearCampaign();
  lastRoster = null;
  debugExtra = '';
  dropMap();
  return doJoin(0);
}

fillCopy();
buildLegend();
wire();
battle = createBattle({
  $, DEBUG, music, show, showError, handleError, note, enterHub, enterCampaign,
  reloadRoster, staleCampaign, startFresh,
  genericText: COPY.generic,
  getRoster: () => lastRoster,
  setDebugExtra: text => { debugExtra = text; renderDebug(); },
});
loadWallet().then(
  () => { mwaState = 'ok'; renderDebug(); },
  e => { mwaState = `failed: ${e && e.message}`; renderDebug(); },
);
boot();
