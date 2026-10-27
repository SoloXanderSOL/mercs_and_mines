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
    this.retryAfter = retryAfter; // whole seconds, on a 429
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

export const store = {
  session: () => readKey(SESSION),                       // {token_id, expires_at}
  setSession: t => writeKey(SESSION, { token_id: t.token_id, expires_at: t.expires_at }),
  campaign: () => readKey(CAMPAIGN),                     // {campaign_id, commander_id}
  setCampaign: c => writeKey(CAMPAIGN, { campaign_id: c.campaign_id, commander_id: c.commander_id }),
  roster: id => readKey(rosterKey(id)),
  setRoster: (id, body) => writeKey(rosterKey(id), body),
  sector: id => readKey(sectorKey(id)),                  // the sector route's body (BP-3a-2)
  setSector: (id, body) => writeKey(sectorKey(id), body),
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
export const counters = { roster: 0, sector: 0, join: 0 };

// Every request gives up after this long and is shown as offline: a stalled connection (booth
// Wi-Fi, a captive portal) must end on a screen with a button. A deadline, not a retry.
const FETCH_DEADLINE_MS = 15000;

function countRequest(path) {
  if (path === '/api/campaign/join') counters.join += 1;
  else if (path.endsWith('/roster')) counters.roster += 1;
  else if (path.endsWith('/sector')) counters.sector += 1;
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
  if (res.status === 429) {
    const n = parseInt(res.headers.get('Retry-After') || '', 10);
    retryAfter = Number.isFinite(n) && n > 0 ? n : 60;
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
