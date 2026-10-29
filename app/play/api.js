// Server calls and per-tab storage for the Seeker client (BP-3a-1).
//
// One fetch wrapper: every non-2xx answer becomes an ApiError whose `kind` the screens map
// to copy. Nothing here retries on a timer; a 500 is retried once, immediately, and only
// where the caller allows it.

export class ApiError extends Error {
  constructor(kind, status, message, retryAfter = null) {
    super(message);
    this.name = 'ApiError';
    this.kind = kind;             // offline | rate | unauth | bad | forbidden | notfound | conflict | unavailable | server | other
    this.status = status;         // 0 when the request never reached the server
    this.retryAfter = retryAfter; // whole seconds, on a 429 (60 if absent) or a 503 that sends one
  }
}

const KIND_BY_STATUS = {
  400: 'bad', 401: 'unauth', 403: 'forbidden', 404: 'notfound',
  409: 'conflict', 413: 'bad', 415: 'bad', 422: 'bad', 429: 'rate', 500: 'server', 503: 'unavailable',
};

// ── per-tab storage ─────────────────────────────────────────────────────────
// sessionStorage, mirrored in memory so the page still works when storage throws
// (private mode, blocked site data).
const memory = new Map();

function readKey(key) {
  try {
    const raw = sessionStorage.getItem(key);
    if (raw !== null) return JSON.parse(raw);
  } catch { /* fall through to memory */ }
  return memory.has(key) ? memory.get(key) : null;
}

function writeKey(key, value) {
  memory.set(key, value);
  try { sessionStorage.setItem(key, JSON.stringify(value)); } catch { /* memory only */ }
}

function removeKey(key) {
  memory.delete(key);
  try { sessionStorage.removeItem(key); } catch { /* memory only */ }
}

const SESSION = 'mm.session';
const CAMPAIGN = 'mm.campaign';
const rosterKey = id => `mm.roster.${id}`;
const sectorKey = id => `mm.sector.${id}`; // ~87 KB; sessionStorage allows ~5 MB per origin
const MISSIONS = 'mm.missions'; // GET /api/missions, static: once per tab (BP-3b-2)
const UNITS = 'mm.units';       // GET /api/units, static: once per tab
const genesisKey = w => `mm.genesis.${w}`; // {holds} from a 200 only, per wallet (BP-3c-2)
const CHECKOUT_REF = 'mm.checkout.ref'; // {reference, wallet}: this tab's unfinished checkout (BP-3c-2b)
const offeredKey = id => `mm.checkout.offered.${id}`; // the checkout was offered after this campaign's AAR
const MUTE = 'mm.mute'; // the music's mute (BP-3c-3), kept for the tab

export const store = {
  session: () => readKey(SESSION),                       // {token_id, expires_at}
  setSession: t => writeKey(SESSION, {
    token_id: t.token_id, expires_at: t.expires_at, ...(t.wallet_address ? { wallet_address: t.wallet_address } : {}),
  }),
  campaign: () => readKey(CAMPAIGN),                     // {campaign_id, commander_id}
  setCampaign: c => writeKey(CAMPAIGN, { campaign_id: c.campaign_id, commander_id: c.commander_id }),
  roster: id => readKey(rosterKey(id)),
  setRoster: (id, body) => writeKey(rosterKey(id), body),
  sector: id => readKey(sectorKey(id)),                  // the sector route's body (BP-3a-2)
  setSector: (id, body) => writeKey(sectorKey(id), body),
  missions: () => readKey(MISSIONS),
  setMissions: body => writeKey(MISSIONS, body),
  units: () => readKey(UNITS),
  setUnits: body => writeKey(UNITS, body),
  genesis: wallet => readKey(genesisKey(wallet)),
  setGenesis: (wallet, answer) => writeKey(genesisKey(wallet), answer),
  // The checkout's two keys live outside clearAll(): a 401 and a re-sign-in keep them.
  checkoutRef: () => readKey(CHECKOUT_REF),
  setCheckoutRef: r => writeKey(CHECKOUT_REF, { reference: r.reference, wallet: r.wallet }),
  clearCheckoutRef: reference => { if (readKey(CHECKOUT_REF)?.reference === reference) removeKey(CHECKOUT_REF); },
  offered: id => readKey(offeredKey(id)) === true,
  setOffered: id => writeKey(offeredKey(id), true),
  // The mute is outside clearAll() too: it survives a 401 and a re-sign-in.
  muted: () => readKey(MUTE) === true,
  setMuted: b => writeKey(MUTE, !!b),
  clearCampaign() {
    const c = readKey(CAMPAIGN);
    if (c && c.campaign_id) {
      removeKey(rosterKey(c.campaign_id));
      removeKey(sectorKey(c.campaign_id));
    }
    removeKey(CAMPAIGN);
  },
  clearAll() {
    this.clearCampaign();
    removeKey(SESSION);
  },
};

// Request counters for the ?debug=1 footer, counted per request (a retried 500 counts twice).
// `mission` is counted by the Deploy tap (battle.js), once per POST.
export const counters = { roster: 0, sector: 0, join: 0, missions: 0, units: 0, mission: 0 };

// Every request gives up after this long and is shown as offline: a stalled connection (booth
// Wi-Fi, a captive portal) must end on a screen with a button. A deadline, not a retry.
const FETCH_DEADLINE_MS = 15000;

function countRequest(path) {
  if (path === '/api/campaign/join') counters.join += 1;
  else if (path.endsWith('/roster')) counters.roster += 1;
  else if (path.endsWith('/sector')) counters.sector += 1;
  else if (path === '/api/missions') counters.missions += 1;
  else if (path === '/api/units') counters.units += 1;
}

// TypeError (no network), TimeoutError (the deadline) and AbortError all mean the same to the
// player: Command can't be reached. The name stays in the message for the debug footer.
function offline(e) {
  return new ApiError('offline', 0, `${e && e.name ? e.name : 'Error'}: ${e && e.message ? e.message : 'network error'}`);
}

// ── the wrapper ─────────────────────────────────────────────────────────────
async function send(method, path, { body, auth }) {
  const headers = { accept: 'application/json' };
  if (body !== undefined) headers['content-type'] = 'application/json';
  if (auth) {
    const s = store.session();
    if (s && s.token_id) headers.authorization = `Bearer ${s.token_id}`;
  }
  countRequest(path);
  let res;
  let text;
  try {
    res = await fetch(path, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(FETCH_DEADLINE_MS),
    });
    text = await res.text(); // the deadline covers the body too
  } catch (e) {
    throw offline(e);
  }
  let json = null;
  try { json = text ? JSON.parse(text) : null; } catch { /* axum rejections are plain text */ }
  if (res.ok) return { status: res.status, body: json };

  const message = (json && typeof json.error === 'string') ? json.error : text;
  const kind = KIND_BY_STATUS[res.status] || 'other';
  let retryAfter = null;
  if (res.status === 429 || res.status === 503) {
    const n = parseInt(res.headers.get('Retry-After') || '', 10);
    retryAfter = Number.isFinite(n) && n > 0 ? n : (res.status === 429 ? 60 : null);
  }
  throw new ApiError(kind, res.status, message, retryAfter);
}

export async function request(method, path, { body, auth = true, retry500 = true } = {}) {
  try {
    return await send(method, path, { body, auth });
  } catch (e) {
    if (retry500 && e instanceof ApiError && e.kind === 'server') {
      return send(method, path, { body, auth }); // once, at most
    }
    throw e;
  }
}

// ── routes ──────────────────────────────────────────────────────────────────
// The two sign-in posts never retry a 500: verify consumes the challenge on its first call.
export async function postChallenge(walletAddress) {
  const r = await request('POST', '/api/auth/challenge', { body: { wallet_address: walletAddress }, auth: false, retry500: false });
  return r.body; // {challenge, expires_in}
}

export async function postVerify(walletAddress, challenge, signature) {
  const r = await request('POST', '/api/auth/verify', {
    body: { wallet_address: walletAddress, challenge, signature }, auth: false, retry500: false,
  });
  return r.body; // SessionToken: {token_id, expires_at, issued_at, account_id, wallet_address}
}

export async function getSession() {
  const r = await request('GET', '/api/auth/session');
  return r.body;
}

export async function postJoin() {
  const r = await request('POST', '/api/campaign/join'); // 201 Joined or 200 AlreadyMember
  return r.body; // {campaign_id, commander_id, foreman_id, section_id, crew_id}
}

export async function getRoster(campaignId) {
  const r = await request('GET', `/api/campaign/${encodeURIComponent(campaignId)}/roster`);
  return r.body; // {campaign_id, commanders, foremen, sections, crews}
}

export async function getSector(campaignId) {
  const r = await request('GET', `/api/campaign/${encodeURIComponent(campaignId)}/sector`);
  return r.body; // {campaign_id, sector_id, terrain, magma_veins, gateway (or null)}
}

// The two static lists (BP-3b-2), fetched once per tab and kept in sessionStorage.
export async function getMissions() {
  const cached = store.missions();
  if (cached) return cached;
  const r = await request('GET', '/api/missions');
  store.setMissions(r.body); // {GUARD_DUTY: MissionDefinition, …}
  return r.body;
}

export async function getUnits() {
  const cached = store.units();
  if (cached) return cached;
  const r = await request('GET', '/api/units');
  store.setUnits(r.body); // {KEY: UnitDefinition, …}
  return r.body;
}

// The Genesis Token lookup (BP-4). It gates nothing (ODQ-53 (a)): the screen shows a badge or
// courtesy copy. Only `holds_genesis_token` is read; the mint is never read or shown.
export async function getGenesis() {
  const r = await request('GET', '/api/genesis');
  return r.body;
}

// The server's build, for the ?debug=1 footer only (BP-3c-4). Unauthenticated and never retried;
// the play worker never intercepts it, so the answer always comes from the server.
export async function getVersion() {
  const r = await request('GET', '/api/version', { auth: false, retry500: false });
  return r.body; // {build_sha, version}
}

// The checkout (BP-5's routes, BP-3c-2b's client). The POST takes no body and is safe to repeat:
// the server hands back the wallet's unpaid intent. The GET says where a payment stands.
export async function postCheckoutIntent() {
  const r = await request('POST', '/api/checkout/intent');
  return r.body; // {reference, amount, decimals, recipient, mint, cluster, expires_in_secs, status}
}

export async function getCheckoutIntent(reference) {
  const r = await request('GET', `/api/checkout/intent/${encodeURIComponent(reference)}`);
  return r.body; // the same, with status pending | confirmed | mismatch
}

// Never retried: a 500 may come after the input is logged, so a retry would deploy again.
export async function postMission(body) {
  const r = await request('POST', '/api/mission/resolve', { body, retry500: false });
  return r.body; // {session_id, report, commander: {kia, glorious, stress, xp, old_rank, new_rank, stress_change, xp_gained}}
}

// "New run": retire this campaign so the next join starts a fresh one. Never retried.
export async function postAbandon(campaignId) {
  const r = await request('POST', `/api/campaign/${encodeURIComponent(campaignId)}/abandon`, { retry500: false });
  return r.body; // {campaign_id}
}
