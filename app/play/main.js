// Seeker client entry point (BP-3a-1, portrait-first since BP-3a-1b / ODQ-58): title → MWA
// sign-in → join → Commander reveal → hub. One upright layout; nothing is locked or blocked.
//
// Boot and reload rule (spec check §8):
//   0. the working screen first, so the stage is never blank while boot waits;
//   1. no stored session → title;
//   2. otherwise one GET /api/auth/session (not rate-limited); a 401 → title, then sign-in;
//   3. a stored campaign → its cached roster → reveal, with no limited request; the roster is
//      fetched once only if its cache is missing;
//   4. no stored campaign → one join, then one roster fetch.
// A roster 409 (or 404/403: the stored campaign is stale) clears the campaign and joins once
// more; a second one in a row is the error screen. Nothing runs on a timer and nothing retries
// by itself: every retry is a tap.

import { ApiError, store, counters, getSession, postJoin, getRoster } from './api.js';
import { loadWallet, signIn, WalletError } from './wallet.js';
import { renderReveal } from './reveal.js';

// Booth copy, verbatim from the BP-3a-1 and BP-3a-1b prompts.
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
  hubPlaceholder: 'Sector map incoming. Command is still drawing it.',
};

const $ = id => document.getElementById(id);
const SCREENS = ['screen-title', 'screen-signin', 'screen-working', 'screen-reveal', 'screen-hub', 'screen-error'];
const DEBUG = new URLSearchParams(location.search).get('debug') === '1';

let busy = false;           // one wallet or server flow at a time
let pendingNotice = '';     // shown on the sign-in screen when it next opens
let errorRetry = null;      // what the error screen's button runs
let lastRoster = null;      // the last roster, kept in memory too (Reviewer BP-2b-2 F2)
let mwaState = 'loading';
let lastError = '';

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

function showReveal(camp, roster) {
  lastRoster = roster;
  const ok = renderReveal($('reveal-content'), roster, camp.commander_id,
    { heading: COPY.revealHeading, outfit: COPY.revealOutfit });
  if (!ok) {
    lastError = `commander ${camp.commander_id} not in roster`;
    return showError(COPY.generic, () => loadRoster(camp, 1));
  }
  show('screen-reveal');
}

// ── boot ────────────────────────────────────────────────────────────────────
async function boot() {
  $('working-text').textContent = COPY.bootWorking;
  show('screen-working'); // never a blank stage while the session check is in flight
  if (!store.session()) return show('screen-title');
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
    + (lastError ? ` · last error: ${lastError}` : '');
}

// ── wiring ──────────────────────────────────────────────────────────────────
function fillCopy() {
  $('btn-start').textContent = COPY.start;
  $('title-tagline').textContent = COPY.tagline;
  $('btn-signin').textContent = COPY.signinButton;
  $('signin-explainer').textContent = COPY.signinExplainer;
  $('signin-lna').textContent = COPY.signinLna;
  $('hub-text').textContent = COPY.hubPlaceholder;
}

function wire() {
  $('btn-start').addEventListener('click', onStart);
  $('btn-signin').addEventListener('click', onSignIn);
  $('btn-error-retry').addEventListener('click', () => { const r = errorRetry; errorRetry = null; if (r) r(); });
  $('btn-continue').addEventListener('click', () => show('screen-hub'));
  $('btn-back').addEventListener('click', () => {
    const camp = store.campaign();
    if (camp && lastRoster) return showReveal(camp, lastRoster);
    return enterCampaign(0);
  });
  if (DEBUG) $('debug').hidden = false;
}

fillCopy();
wire();
loadWallet().then(
  () => { mwaState = 'ok'; renderDebug(); },
  e => { mwaState = `failed: ${e && e.message}`; renderDebug(); },
);
boot();
