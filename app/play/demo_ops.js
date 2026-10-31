// demo-only (BP-9a, BP-9b): pins, travel and the dig site on the hub map (Breakpoint §4d). Never ported to dev.
//
// Each new sector map gets the campaign's ops board (one GET), drawn as pins over the map. A tap
// on a pin opens its sheet; Go sends the team, and a marker crawls from the gateway to the site
// between the leg's depart_at and arrive_at, read against the server's clock (every body carries
// the server's `now`, so a phone whose clock is off still crawls right). A contract hands off to
// the existing briefing, battle and AAR; Return crawls home. Nothing polls: the only frame driver
// is the overlay asking the map for its next frame mid-leg. Every string goes in through textContent.
//
// BP-9b: at an arrived dig the strip carries demo_dig.js's panel (heat, ore, raid risk, the Crew)
// and Keep drilling / Pull out. A raid or an ambush opens its GUARD_DUTY briefing the first time a
// response shows it, and the next ops read after the fight latches the outcome. Beyond the
// player's taps the client reads the board three times: on the hub's re-entry with an op out, at a
// pulled haul's midpoint (where an ambush reveals) and when a laden leg lands (where the ore banks).

import { request, ApiError, store, getMissions, getRoster } from './api.js';
import { onMapCreated } from './hexmap.js';
import { DIG_COPY, DIG_CSS, createDigPanel, renderDig, hideDig, homewardLines, flash } from './demo_dig.js';

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
let banked = 0;         // the campaign's ore_banked, from the last adopted body
let bankNote = null;    // the one-off "ore banked" line after a laden leg lands
let seenBattle = null;  // the raid or ambush whose briefing has opened once ('raid:<site>', 'ambush:<site>')
let legKey = null;      // the site and leg the two reads below belong to
let midAsked = false;   // the pulled haul's midpoint read has been sent
let landAsked = false;  // the laden leg's arrival read has been sent
let throttleUntil = 0;  // a 429's Retry-After, in browser ms: no call and no button until then
let throttleSecs = 0;
let hubWasShown = false;

let strip = null, titleNode = null, lineNodes = [], actions = null, buttons = [];
let dig = null, bankChip = null, endNote = null;
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
const held = () => busy || Date.now() < throttleUntil;
const battleOf = o => (o && o.dig && (o.dig.raid || o.dig.ambush)) || null;
const battleKey = o => (!battleOf(o) ? null : (o.dig.raid ? 'raid:' : 'ambush:') + o.site_id);

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
function setStrip(title, lines, acts, danger) {
  titleNode.hidden = !title;
  titleNode.textContent = title || '';
  titleNode.classList.toggle('danger', Boolean(danger));
  lineNodes.forEach((node, i) => {
    node.hidden = !lines[i];
    node.textContent = lines[i] || '';
  });
  buttons.forEach((b, i) => {
    const a = acts[i];
    b.hidden = !a;
    handlers[i] = a ? a[1] : null;
    b.textContent = a ? a[0] : '';
    b.disabled = Boolean(a && (a[2] || held()));
  });
  actions.hidden = acts.length === 0;
  strip.hidden = false;
}

function hideStrip() {
  if (strip) strip.hidden = true;
}

function refresh() {
  if (!strip) return;
  // While a raid or an ambush waits, the hub's own GUARD_DUTY would count as its fight
  // (Reviewer BP-8b-2 F1), so the mission list is shut until the dig's fight is fought.
  deps.$('btn-missions').disabled = Boolean(battleOf(op));
  bankChip.textContent = fill(DIG_COPY.bankedChip, { total: banked });
  bankChip.hidden = !(banked > 0);
  shownPhase = null;
  shownSecs = null;
  const rate = Date.now() < throttleUntil ? fill(DIG_COPY.rate, { n: throttleSecs }) : '';
  if (op && op.leg === 'outbound' && op.dig && progress(op) >= 1) return showDig(op);
  hideDig(dig);
  if (op) {
    const site = siteOf(op.site_id);
    const title = titleOf(site);
    const p = progress(op);
    shownPhase = `${op.leg}:${p >= 1}`;
    if (op.leg === 'homeward') {
      if (op.dig && op.dig.ambush) return setStrip('', [DIG_COPY.ambushWait, rate], [[DIG_COPY.fight, fight]]);
      shownSecs = secsLeft(op);
      const h = homewardLines(op.dig, title, shownSecs);
      if (h) return setStrip(h.title, [...h.lines, rate], [], h.danger);
      return setStrip('', [fill(COPY.homeward, { n: shownSecs }), rate], []);
    }
    if (p < 1) {
      shownSecs = secsLeft(op);
      return setStrip('', [fill(COPY.outbound, { title, n: shownSecs }), rate], [[COPY.ret, null, true]]);
    }
    if (site && site.kind === 'contract') {
      return setStrip('', [fill(COPY.atContract, { title }), rate],
        [[COPY.fight, () => deps.openMission(site.mission_id, () => deps.enterHub())], [COPY.ret, sendReturn]]);
    }
    return setStrip('', [fill(COPY.onSite, { title }), rate], [[COPY.ret, sendReturn]]);
  }
  if (selected) return showSheet(selected);
  if (bankNote) return setStrip('', [bankNote, rate], [[COPY.close, close]]);
  if (noDigOpen) return setStrip('', [COPY.noDig, rate], [[COPY.close, close]]);
  if (rate) return setStrip('', [rate], []);
  hideStrip();
}

// The Hold-or-Fold screen: the panel stays up for as long as the team is on site, so heat and ore
// are never behind a tap. With ore on site the only way home is Pull out: walking away would
// throw the pile away, so that button is offered only at 0 ore.
function showDig(o) {
  const site = siteOf(o.site_id);
  const title = titleOf(site);
  const rate = Date.now() < throttleUntil ? fill(DIG_COPY.rate, { n: throttleSecs }) : '';
  shownPhase = `${o.leg}:true`;
  renderDig(dig, o.dig, store.roster(campaignId));
  if (o.dig.raid) return setStrip(title, [fill(DIG_COPY.raidWait, { title }), rate], [[DIG_COPY.fight, fight]]);
  const second = o.dig.ore_on_site > 0 ? [DIG_COPY.pull, sendPull] : [DIG_COPY.home, sendReturn];
  return setStrip(title, [rate], [[DIG_COPY.drill, sendDrill], second]);
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
  bankNote = null;
  hideStrip();
}

// The raid's or the ambush's GUARD_DUTY briefing, with the dig's lead line over the mission's
// own text. The Commander is the campaign's stored one, the one the dig start picked.
async function fight() {
  const b = battleOf(op);
  if (!b) return;
  const lead = op.dig.raid
    ? fill(DIG_COPY.raidLead, { title: titleOf(siteOf(op.site_id)) })
    : fill(DIG_COPY.ambushLead, { carried: op.dig.carried });
  await deps.openMission(b.mission_id, () => deps.enterHub());
  if (!deps.$('screen-briefing').hidden) deps.$('briefing-text').textContent = lead;
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
    // An ambushed team waits at the midpoint until its fight is latched; the leg's times run on
    // meanwhile, so the marker is pinned rather than read from them (Reviewer BP-8b-2 F2).
    const ambush = Boolean(op.dig && op.dig.ambush);
    const p = ambush ? 0.5 : progress(op);
    const home = !ambush && op.leg === 'homeward' && p >= 1;
    if (home && !(op.dig && op.dig.carried > 0)) {
      op = null; // home with nothing to bank: the server clears it on its next read, so nothing is sent
      selected = null;
      refresh();
    } else {
      if (home && !landAsked) { landAsked = true; load(); } // a laden leg landed: that read banks the ore
      if (!ambush && op.leg === 'homeward' && op.dig && op.dig.ended_by === 'pulled' && p >= 0.5 && !midAsked) {
        midAsked = true; // an ambush on this haul reveals on the first read at or after the midpoint
        load();
      }
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
      if (phase !== shownPhase || (p < 1 && !ambush && secsLeft(op) !== shownSecs)) refresh();
      if (p < 1 && !ambush && hubShown()) m.requestFrame();
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
  bankNote = null;
  if (selected) showSheet(selected);
  else refresh();
}

// ── calls ────────────────────────────────────────────────────────────────────
function adopt(body) {
  if (!body || body.campaign_id !== campaignId) return;
  offset = Date.parse(body.now) - Date.now();
  sites = Array.isArray(body.sites) ? body.sites : [];
  gateway = body.gateway || null;
  const before = op;
  const was = banked;
  op = body.active_op || null;
  banked = Number(body.ore_banked) || 0;
  if (op) selected = null;
  else if (selected) selected = siteOf(selected.site_id); // the board may have moved on (Reviewer BP-9a F4)
  const key = op ? `${op.site_id}:${op.leg}` : null;
  if (key !== legKey) { legKey = key; midAsked = false; landAsked = false; }
  if (op) bankNote = null;
  else if (banked > was && before && before.leg === 'homeward' && before.dig && before.dig.carried > 0) {
    bankNote = fill(DIG_COPY.banked, { gained: banked - was, total: banked });
  }
  if (!op && gateway && !noDigOffered && !sites.some(st => st.kind === 'dig')) {
    noDigOffered = true;
    noDigOpen = true;
  }
  const endedNow = op && op.dig ? op.dig.ended_by : null;
  const endedBefore = before && before.dig ? before.dig.ended_by : null;
  const melted = endedNow === 'meltdown' && endedBefore !== 'meltdown';
  // A raid or an ambush opens its briefing on the response that first shows it, never again
  // after Back: the strip's Fight stays until the fight is fought.
  const battle = battleKey(op);
  const opening = Boolean(battle && battle !== seenBattle && hubShown());
  seenBattle = battle;
  if (map) map.requestFrame();
  refresh();
  if (melted) { flash(strip); rereadRoster(); }
  if (opening) fight();
}

// A meltdown cost the Crew heads: the stored roster is read again once, so the Crew line and a
// reload's reveal show the new headcount. Under the roster limiters, not the ops ones.
async function rereadRoster() {
  const id = campaignId;
  try {
    const body = await getRoster(id);
    if (id !== campaignId) return;
    store.setRoster(id, body);
    refresh();
  } catch { /* the line keeps the old count; the next roster load corrects it */ }
}

const retry = () => { deps.enterHub(); load(); };

function stale(e) {
  if (e instanceof ApiError && e.kind === 'conflict' && e.message === 'campaign is not active') {
    deps.staleCampaign(true);
    return true;
  }
  return false;
}

// A 429 waits out its Retry-After on the strip, never on the error screen; then one read.
function rated(e) {
  if (!(e instanceof ApiError) || e.kind !== 'rate') return false;
  throttleSecs = e.retryAfter || 60;
  throttleUntil = Date.now() + throttleSecs * 1000;
  refresh();
  setTimeout(() => { throttleUntil = 0; refresh(); load(); }, throttleSecs * 1000);
  return true;
}

async function load() {
  const id = campaignId;
  if (!id || held()) return; // one read at a time (Reviewer BP-9a F3)
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
  if (stale(err) || rated(err)) return;
  if (err instanceof ApiError && (err.kind === 'notfound' || err.kind === 'forbidden')) return deps.staleCampaign();
  deps.handleError(err, retry);
}

// One POST; a refusal other than a stale campaign re-reads the board once and shows its state.
async function send(path, body) {
  const id = campaignId;
  if (held() || !id) return;
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
  if (stale(err) || rated(err)) return;
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

function sendDrill() {
  send(opsPath(campaignId) + '/drill', undefined);
}

function sendPull() {
  send(opsPath(campaignId) + '/pull', undefined);
}

// ── the hub's re-entry and the end screen ─────────────────────────────────────
// Both are seen through their `hidden` attribute, which main.js's show() sets, so no keeper code
// calls in here. Back on the hub with an op out, one read latches a fought raid or ambush (the
// fight's AAR returns to the hub without one) and shows anything that moved while away.
function watch() {
  const hub = deps.$('screen-hub'), end = deps.$('screen-end');
  hubWasShown = !hub.hidden;
  new MutationObserver(() => {
    const shown = !hub.hidden;
    if (shown && !hubWasShown && op) load();
    hubWasShown = shown;
  }).observe(hub, { attributes: true, attributeFilter: ['hidden'] });
  new MutationObserver(() => { if (!end.hidden) writeEndNote(); }).observe(end, { attributes: true, attributeFilter: ['hidden'] });
}

// New run starts a new campaign, and ore belongs to the campaign: the end screen says what stays behind.
function writeEndNote() {
  const camp = store.campaign();
  let text = '';
  if (camp && camp.campaign_id === campaignId) {
    if (banked > 0) text = fill(DIG_COPY.endBanked, { total: banked });
    else if (op && op.dig && op.dig.ore_on_site + op.dig.carried > 0) text = DIG_COPY.endOut;
  }
  endNote.textContent = text;
  endNote.hidden = !text;
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
  banked = 0;
  bankNote = null;
  seenBattle = null;
  legKey = null;
  midAsked = false;
  landAsked = false;
  refresh();
  m.overlay = draw;
  m.onTap = tap;
  load();
}

// `d`: main.js's $, show, handleError, staleCampaign, enterHub and the battle's single briefing.
export function mountOps(d) {
  deps = d;
  const style = document.createElement('style');
  style.textContent = CSS + DIG_CSS;
  document.head.append(style);
  strip = el('div', 'ops-strip');
  strip.hidden = true;
  titleNode = el('h2', 'ops-title');
  dig = createDigPanel();
  lineNodes = [el('p', 'ops-line'), el('p', 'ops-line'), el('p', 'ops-line')];
  actions = el('div', 'ops-actions');
  buttons = [el('button', 'btn btn-primary'), el('button', 'btn')];
  buttons.forEach((b, i) => {
    b.type = 'button';
    b.addEventListener('click', () => { if (handlers[i]) handlers[i](); });
    actions.append(b);
  });
  strip.append(titleNode, dig.node, ...lineNodes, actions);
  bankChip = el('div', 'ops-banked');
  bankChip.hidden = true;
  deps.$('screen-hub').append(strip, bankChip);
  endNote = el('p', 'end-ops');
  endNote.hidden = true;
  deps.$('btn-new-run').before(endNote);
  watch();
  onMapCreated(attach);
}
