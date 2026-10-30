// demo-only (BP-9a): pins and travel on the hub map (Breakpoint §4d). Never ported to dev.
//
// Each new sector map gets the campaign's ops board (one GET), drawn as pins over the map. A tap
// on a pin opens its sheet; Go sends the team, and a marker crawls from the gateway to the site
// between the leg's depart_at and arrive_at, read against the server's clock (every body carries
// the server's `now`, so a phone whose clock is off still crawls right). A contract hands off to
// the existing briefing, battle and AAR; Return crawls home. Arrival is computed here: nothing
// polls, and the only frame driver is the overlay asking the map for its next frame mid-leg.
// Every string goes in through textContent.

import { request, ApiError, store, getMissions } from './api.js';
import { onMapCreated } from './hexmap.js';

// Booth copy, verbatim from the BP-9a prompt.
const COPY = {
  digTitle: { 1: 'Thin vein', 3: 'Rich vein' },
  digTitleOther: 'Vein',
  digReward: { 1: 'Poor ore, close to home', 3: 'Rich ore, double yield' },
  reward: '{credits} credits · {ore} ore',
  rewardNoOre: '{credits} credits',
  distance: '{n} hexes from the gateway',
  travel: '{n} s each way',
  go: 'Go',
  close: 'Close',
  ret: 'Return',
  fight: 'Fight',
  outbound: 'Travelling to {title}. {n} s',
  onSite: 'On site at {title}.',
  atContract: 'At {title}. Fight, or head home.',
  homeward: 'Heading home. {n} s',
  noDig: 'No workable veins near your gateway. Take a contract.',
};

const COLOUR = { dig: '#ff8c2a', ring: '#ffd27a', contract: '#6fd3ff', edge: '#0b0b0c', marker: '#ffffff', route: 'rgba(255, 255, 255, 0.7)' };

const CSS = `
.ops-strip { position: absolute; left: 3cqw; right: 3cqw; bottom: 17cqw; padding: 3cqw; display: flex; flex-direction: column; gap: 1.5cqw; text-align: left; background: rgba(11, 11, 12, 0.94); border: 0.4cqw solid var(--panel-edge); border-radius: 2cqw; }
.ops-strip[hidden], .ops-title[hidden], .ops-line[hidden], .ops-actions[hidden], .ops-actions .btn[hidden] { display: none; }
.ops-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 5.5cqw; line-height: 1.1; color: var(--accent); }
.ops-line { margin: 0; font-size: 3.9cqw; line-height: 1.35; }
.ops-actions { display: flex; gap: 2cqw; margin-top: 1cqw; }
.ops-actions .btn { flex: 1; width: auto; max-width: none; min-height: 11cqw; padding: 0 2cqw; font-size: 4.4cqw; }
`;

const opsPath = id => '/api/campaign/' + encodeURIComponent(id) + '/ops';
const fill = (text, values) => text.replace(/\{(\w+)\}/g, (m, k) => (k in values ? String(values[k]) : m));

let deps = null;
let map = null;         // the map the pins are drawn on
let campaignId = null;  // the campaign that map belongs to
let sites = [];
let gateway = null;
let op = null;          // the server's active_op, kept until a homeward leg arrives
let offset = 0;         // server clock minus browser clock, from the last adopted body
let selected = null;    // the site whose sheet is open
let noDigOpen = false;  // the no-dig line is showing
let noDigOffered = false;
let busy = false;       // one ops call at a time
let shownPhase = null;  // what the strip shows mid-leg, so it is rewritten only on a change
let shownSecs = null;

let strip = null, titleNode = null, lineNodes = [], actions = null, buttons = [];
const handlers = [null, null];

// ── time and progress ────────────────────────────────────────────────────────
const serverNow = () => Date.now() + offset;

function progress(o) {
  const depart = Date.parse(o.depart_at), arrive = Date.parse(o.arrive_at);
  const span = arrive - depart;
  if (!(span > 0)) return 1;
  return Math.min(1, Math.max(0, (serverNow() - depart) / span));
}

const secsLeft = o => Math.max(0, Math.ceil((Date.parse(o.arrive_at) - serverNow()) / 1000));
const siteOf = id => sites.find(s => s.site_id === id) || null;
const hubShown = () => !deps.$('screen-hub').hidden;

function titleOf(site) {
  if (!site) return COPY.digTitleOther;
  if (site.kind === 'contract') return site.name || site.mission_id || '';
  return COPY.digTitle[site.tier] || COPY.digTitleOther;
}

// ── the strip ────────────────────────────────────────────────────────────────
function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

// `lines`: up to three strings; `acts`: up to two [text, onClick, disabled] buttons.
function setStrip(title, lines, acts) {
  titleNode.hidden = !title;
  titleNode.textContent = title || '';
  lineNodes.forEach((node, i) => {
    node.hidden = !lines[i];
    node.textContent = lines[i] || '';
  });
  buttons.forEach((b, i) => {
    const a = acts[i];
    b.hidden = !a;
    handlers[i] = a ? a[1] : null;
    b.textContent = a ? a[0] : '';
    b.disabled = Boolean(a && (a[2] || busy));
  });
  actions.hidden = acts.length === 0;
  strip.hidden = false;
}

function hideStrip() {
  if (strip) strip.hidden = true;
}

function refresh() {
  if (!strip) return;
  shownPhase = null;
  shownSecs = null;
  if (op) {
    const site = siteOf(op.site_id);
    const title = titleOf(site);
    const p = progress(op);
    shownPhase = `${op.leg}:${p >= 1}`;
    if (op.leg === 'homeward') {
      shownSecs = secsLeft(op);
      return setStrip('', [fill(COPY.homeward, { n: shownSecs })], []);
    }
    if (p < 1) {
      shownSecs = secsLeft(op);
      return setStrip('', [fill(COPY.outbound, { title, n: shownSecs })], [[COPY.ret, null, true]]);
    }
    if (site && site.kind === 'contract') {
      return setStrip('', [fill(COPY.atContract, { title })],
        [[COPY.fight, () => deps.openMission(site.mission_id, () => deps.enterHub())], [COPY.ret, sendReturn]]);
    }
    return setStrip('', [fill(COPY.onSite, { title })], [[COPY.ret, sendReturn]]);
  }
  if (selected) return showSheet(selected);
  if (noDigOpen) return setStrip('', [COPY.noDig], [[COPY.close, close]]);
  hideStrip();
}

function showSheet(site) {
  const lines = [fill(COPY.distance, { n: Math.round(site.distance) }), fill(COPY.travel, { n: Math.round(site.travel_secs) })];
  if (site.kind === 'dig') {
    if (COPY.digReward[site.tier]) lines.push(COPY.digReward[site.tier]);
  } else {
    // The reward is the mission's, from the cached list; without it the line is left out.
    getMissions().then(ms => {
      const m = ms && ms[site.mission_id];
      if (!m || op || selected !== site) return;
      const text = m.ore_reward > 0
        ? fill(COPY.reward, { credits: m.credit_reward, ore: m.ore_reward })
        : fill(COPY.rewardNoOre, { credits: m.credit_reward });
      lineNodes[2].textContent = text;
      lineNodes[2].hidden = false;
    }, () => {});
  }
  setStrip(titleOf(site), lines, [[COPY.go, sendStart], [COPY.close, close]]);
}

function close() {
  selected = null;
  noDigOpen = false;
  hideStrip();
}

// ── the overlay, drawn last in every map frame ───────────────────────────────
function pin(ctx, site, x, y, r, line) {
  ctx.lineWidth = line;
  ctx.strokeStyle = COLOUR.edge;
  ctx.beginPath();
  if (site.kind === 'contract') {
    if (ctx.roundRect) ctx.roundRect(x - r, y - r, 2 * r, 2 * r, r * 0.35);
    else ctx.rect(x - r, y - r, 2 * r, 2 * r);
    ctx.fillStyle = COLOUR.contract;
    ctx.fill();
    ctx.stroke();
    return;
  }
  const rich = site.tier === 3;
  const rad = rich ? r * 4 / 3 : r;
  ctx.arc(x, y, rad, 0, Math.PI * 2);
  ctx.fillStyle = COLOUR.dig;
  ctx.fill();
  ctx.stroke();
  if (rich) {
    ctx.beginPath();
    ctx.arc(x, y, rad + line * 1.5, 0, Math.PI * 2);
    ctx.strokeStyle = COLOUR.ring;
    ctx.stroke();
  }
}

function draw(ctx, m) {
  if (m !== map) return;
  const s = m.view.s;
  const r = Math.max(6, s * 0.55);
  const line = Math.max(1, s * 0.08);
  const pad = r * 2;
  const onScreen = (x, y) => x > -pad && y > -pad && x < m.w + pad && y < m.h + pad;
  ctx.save();
  const site = op && siteOf(op.site_id);
  if (site && gateway) {
    const [gx, gy] = m.hexToScreen(gateway.q, gateway.r);
    const [sx, sy] = m.hexToScreen(site.q, site.r);
    ctx.setLineDash([4, 4]);
    ctx.lineWidth = Math.max(1, s * 0.1);
    ctx.strokeStyle = COLOUR.route;
    ctx.beginPath();
    ctx.moveTo(gx, gy);
    ctx.lineTo(sx, sy);
    ctx.stroke();
    ctx.setLineDash([]);
  }
  for (const st of sites) {
    const [x, y] = m.hexToScreen(st.q, st.r);
    if (onScreen(x, y)) pin(ctx, st, x, y, r, line);
  }
  if (op) {
    const p = progress(op);
    if (p >= 1 && op.leg === 'homeward') {
      op = null; // home: the server clears it on its next read, so nothing is sent
      selected = null;
      refresh();
    } else {
      if (site && gateway) {
        const [from, to] = op.leg === 'outbound' ? [gateway, site] : [site, gateway];
        const [x, y] = m.hexToScreen(from.q + (to.q - from.q) * p, from.r + (to.r - from.r) * p);
        ctx.beginPath();
        ctx.arc(x, y, Math.max(5, s * 0.45), 0, Math.PI * 2);
        ctx.fillStyle = COLOUR.marker;
        ctx.fill();
        ctx.lineWidth = line;
        ctx.strokeStyle = COLOUR.edge;
        ctx.stroke();
      }
      const phase = `${op.leg}:${p >= 1}`;
      if (phase !== shownPhase || (p < 1 && secsLeft(op) !== shownSecs)) refresh();
      if (p < 1 && hubShown()) m.requestFrame();
    }
  }
  ctx.restore();
}

// ── taps ─────────────────────────────────────────────────────────────────────
// The pin on the exact hex, else the nearest pin centre within max(22, s) CSS px, else none.
function pick(x, y) {
  const hex = map.hexAt(x, y);
  if (hex) {
    const exact = sites.find(st => st.q === hex.q && st.r === hex.r);
    if (exact) return exact;
  }
  const rect = map.canvas.getBoundingClientRect();
  const px = x - rect.left, py = y - rect.top;
  const reach = Math.max(22, map.view.s);
  let best = null, bestD = Infinity;
  for (const st of sites) {
    const [sx, sy] = map.hexToScreen(st.q, st.r);
    const d = Math.hypot(sx - px, sy - py);
    if (d <= reach && d < bestD) { best = st; bestD = d; }
  }
  return best;
}

function tap(x, y) {
  if (op) return; // the strip stays while the team is out
  selected = pick(x, y);
  noDigOpen = false;
  if (selected) showSheet(selected);
  else hideStrip();
}

// ── calls ────────────────────────────────────────────────────────────────────
function adopt(body) {
  if (!body || body.campaign_id !== campaignId) return;
  offset = Date.parse(body.now) - Date.now();
  sites = Array.isArray(body.sites) ? body.sites : [];
  gateway = body.gateway || null;
  op = body.active_op || null;
  if (op) selected = null;
  if (!op && gateway && !noDigOffered && !sites.some(st => st.kind === 'dig')) {
    noDigOffered = true;
    noDigOpen = true;
  }
  if (map) map.requestFrame();
  refresh();
}

const retry = () => { deps.enterHub(); load(); };

function stale(e) {
  if (e instanceof ApiError && e.kind === 'conflict' && e.message === 'campaign is not active') {
    deps.staleCampaign(true);
    return true;
  }
  return false;
}

async function load() {
  const id = campaignId;
  if (!id) return;
  busy = true;
  refresh();
  let body = null, err = null;
  try {
    body = (await request('GET', opsPath(id))).body;
  } catch (e) {
    err = e;
  }
  busy = false;
  if (id !== campaignId) return;
  if (!err) return adopt(body);
  refresh();
  if (stale(err)) return;
  if (err instanceof ApiError && (err.kind === 'notfound' || err.kind === 'forbidden')) return deps.staleCampaign();
  deps.handleError(err, retry);
}

// One POST; a refusal other than a stale campaign re-reads the board once and shows its state.
async function send(path, body) {
  const id = campaignId;
  if (busy || !id) return;
  busy = true;
  refresh();
  let res = null, err = null;
  try {
    res = (await request('POST', path, { body, retry500: false })).body;
  } catch (e) {
    err = e;
  }
  busy = false;
  if (id !== campaignId) return;
  if (!err) return adopt(res);
  refresh();
  if (stale(err)) return;
  if (err instanceof ApiError && (err.kind === 'conflict' || (err.kind === 'bad' && err.message === 'unknown site'))) return load();
  deps.handleError(err, retry);
}

function sendStart() {
  if (!selected) return;
  send(opsPath(campaignId) + '/start', { site_id: selected.site_id });
}

function sendReturn() {
  send(opsPath(campaignId) + '/return', undefined);
}

// ── mounting ─────────────────────────────────────────────────────────────────
function attach(m) {
  map = m;
  const camp = store.campaign();
  campaignId = camp ? camp.campaign_id : null;
  sites = [];
  gateway = null;
  op = null;
  offset = 0;
  selected = null;
  noDigOpen = false;
  noDigOffered = false;
  busy = false;
  hideStrip();
  m.overlay = draw;
  m.onTap = tap;
  load();
}

// `d`: main.js's $, show, handleError, staleCampaign, enterHub and the battle's single briefing.
export function mountOps(d) {
  deps = d;
  const style = document.createElement('style');
  style.textContent = CSS;
  document.head.append(style);
  strip = el('div', 'ops-strip');
  strip.hidden = true;
  titleNode = el('h2', 'ops-title');
  lineNodes = [el('p', 'ops-line'), el('p', 'ops-line'), el('p', 'ops-line')];
  actions = el('div', 'ops-actions');
  buttons = [el('button', 'btn btn-primary'), el('button', 'btn')];
  buttons.forEach((b, i) => {
    b.type = 'button';
    b.addEventListener('click', () => { if (handlers[i]) handlers[i](); });
    actions.append(b);
  });
  strip.append(titleNode, ...lineNodes, actions);
  deps.$('screen-hub').append(strip);
  onMapCreated(attach);
}
