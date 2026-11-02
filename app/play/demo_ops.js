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
//
// BP-10b: the Base button opens demo_base.js's sheet, where banked ore buys the four buildings
// (POST ops/build, one per tap). Nothing can be built while any op is out, so the sheet says so
// rather than offering a build the server would refuse (Reviewer BP-10a CF2).
//
// BP-11b-1: the op is never looked up through the board (a fought contract leaves it and returns
// elsewhere): the team stands at `active_op.at`, its title comes from a cache of every site seen,
// and a leg after Next mission is drawn from the hex the team left (cached per departure). After a
// fight on site the strip offers Next mission / Head home; Missions opens a list of the board's
// contracts that hands off to their pins; a latched fight re-reads the roster (which the next
// squad reads) and, after a contract, the board once more when it refreshes; the AAR names the
// dead and the Section's count; no soldiers or no Commander left ends the run.
//
// BP-12b: every ops body's `company` (the sickbays and the Trophy Shelf) is kept with the rest,
// heal and replace are two more POSTs, the squad a fight sends leaves the sickbay at base
// (battle.js's setSquadSize), the AAR names the wounded, and demo_company.js renders the Company
// screen from the view registered at the mount.

import { request, ApiError, store, getMissions, getRoster } from './api.js';
import { onMapCreated } from './hexmap.js';
import { setAarNote, setBeforeOffer, setSquadSize } from './battle.js';
import { showCard, cardSeen } from './demo_card.js';
import { mountCompany, dressBriefing, renderCompany, HEAL_CREDITS } from './demo_company.js';
import { DIG_COPY, DIG_CSS, createDigPanel, renderDig, hideDig, homewardLines, flash, bankedLine } from './demo_dig.js';
import { BASE_COPY, BASE_CSS, createBase, renderBase, hideBase } from './demo_base.js';

// Booth copy, verbatim from the BP-9a and BP-11b-1 prompts.
const COPY = {
  digTitle: { 1: 'Thin vein', 3: 'Rich vein' },
  digTitleOther: 'Vein',
  contractSite: 'Contract site',
  digReward: { 1: 'Poor ore, close to home', 3: 'Rich ore, double yield' },
  reward: '{credits} credits · {ore} ore',
  rewardNoOre: '{credits} credits',
  distance: '{n} hexes from the gateway',
  travel: '{n} s each way',
  go: 'Go',
  close: 'Close',
  cancel: 'Cancel',
  ret: 'Return',
  fight: 'Fight',
  next: 'Next mission',
  outbound: 'Travelling to {title}. {n} s',
  onSite: 'On site at {title}.',
  atContract: 'At {title}. Fight, or head home.',
  won: '{title} secured. +{credits} credits banked.',
  wonNoPay: '{title} secured.',
  lost: '{title} failed. The client is not paying.',
  homeward: 'Heading home. {n} s',
  noDig: 'No workable veins near your gateway. Take a contract.',
  noCrew: 'No Crew left to dig. Contracts only.',
  pick: 'Tap a site for the next job.',
  hop: '{n} hexes from here',
  hopSecs: '{s} s on foot',
  leaveOre: 'Leave {ore} ore in the ground?',
  leaveIt: 'Leave it',
  stay: 'Stay',
  listTitle: 'Contracts on offer',
  listSub: 'Pick a contract. The team walks there first.',
  listOut: 'The team is out.',
  listMeta: '{n} hexes · {s} s',
  aarLost: 'Lost: {names}.',
  aarDead: '{name} ({status})',
  aarNone: 'No losses. Command notes the anomaly.',
  aarSection: '{name}: {a} → {b}',
  shop: 'Demo Shop',
  // BP-12b (Wiki copy)
  aarWounded: 'Wounded: {n}, in the sickbay. Patch them up at Your Company.',
  zeroFit: 'Everyone fit to fight is in the sickbay. Patch them up first.',
  zeroFitHealing: 'Nobody is fit to fight yet. Deploy opens when the sickbay lets them go.',
  walkingWounded: 'Nobody is fit to fight. The walking wounded are going anyway.',
};

// Refusals the client keys on by exact text (routes/ops.rs; a server reword changes this block in the same brick).
const REFUSAL = {
  short: 'not enough banked credits',
  out: 'the team is out: replacements report at base',
  sectionFull: 'no room: a Section holds 8',
  crewFull: 'no room: a Crew holds 8',
};

// A12: the relic contract reads as a relic dig (Wiki copy, BP-11b-2). The server's name stays.
const RELIC = {
  mission_id: 'CRASHED_SHIP_RAID',
  name: 'Relic Dig: Crashed Frigate',
  lead: 'A pre-collapse frigate went into the dirt in Sector 6 before anyone now alive was born. The hold is under three metres of soil and a great deal of competitive interest. Dig it out, see off whoever else turns up, and bring back anything that still hums.',
};
const RELIC_SITE = 'contract:CRASHED_SHIP_RAID';
const relicWon = o => Boolean(o && o.site_id === RELIC_SITE && o.fought === 'won');

// The end of run when the server says nobody is left to send (BP-11b-1).
const END = {
  soldiers: { title: 'Section lost', body: 'The 1st Section is gone to the last Rookie. Command has closed the file and opened a new one.' },
  commander: { title: 'No one left to lead', body: 'There is no Commander left to sign the orders. The Company is already hiring.' },
};
const NO_SOLDIERS = 'no soldiers left to send: start a new run';
const NO_COMMANDER = 'no Commander left to lead: start a new run';
const NO_CREW = 'no Crew left to dig: start a new run';

const REFRESH_READ_MS = 46000; // the server's DEMO_SITE_REFRESH_SECS 45 + 1

const COLOUR = { dig: '#ff8c2a', ring: '#ffd27a', contract: '#6fd3ff', edge: '#0b0b0c', marker: '#ffffff', route: 'rgba(255, 255, 255, 0.7)' };

const CSS = `
.ops-strip { position: absolute; left: 3cqw; right: 3cqw; bottom: 17cqw; padding: 3cqw; display: flex; flex-direction: column; gap: 1.5cqw; text-align: left; background: rgba(11, 11, 12, 0.94); border: 0.4cqw solid var(--panel-edge); border-radius: 2cqw; }
.ops-strip[hidden], .ops-title[hidden], .ops-line[hidden], .ops-actions[hidden], .ops-actions .btn[hidden] { display: none; }
.ops-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 5.5cqw; line-height: 1.1; color: var(--accent); }
.ops-line { margin: 0; font-size: 3.9cqw; line-height: 1.35; }
.ops-actions { display: flex; gap: 2cqw; margin-top: 1cqw; }
.ops-actions .btn { flex: 1; width: auto; max-width: none; min-height: 11cqw; padding: 0 2cqw; font-size: 4.4cqw; }
.ops-list[hidden] { display: none; }
.ops-list-rows { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 1.5cqw; }
.ops-list-row { width: 100%; max-width: none; display: flex; flex-direction: column; align-items: flex-start; gap: 0.6cqw; padding: 2cqw 2.5cqw; text-align: left; }
.ops-list-name { font-family: var(--display); font-weight: 800; font-size: 4.4cqw; }
.ops-list-meta { font-family: var(--mono); font-size: 3.4cqw; color: var(--muted); }
.briefing-fit[hidden] { display: none; }
.briefing-fit { margin: 0; font-size: 3.8cqw; line-height: 1.35; color: var(--accent); }
.ops-shop { position: absolute; top: 1.5cqw; left: 3cqw; padding: 1cqw 2.5cqw; font-family: var(--mono); font-size: 3.6cqw; color: var(--accent); background: rgba(11, 11, 12, 0.94); border: 0.3cqw solid var(--accent); border-radius: 1.5cqw; cursor: pointer; }
`;

const opsPath = id => '/api/campaign/' + encodeURIComponent(id) + '/ops';
const walkKey = id => 'mm.demo.walk.' + id;
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
let credits = 0;        // the campaign's credits_banked, from the last adopted body
let company = { sections: [], trophies: [] }; // the last adopted body's company (BP-12a-1)
// A Section's sickbay from the last adopted body (absent = 0 and 0); healing counts as back once the server's clock passes healed_at.
const bayOf = id => {
  const b = company.sections.find(s => s.section_id === id) || { wounded: 0, healing: 0, healed_at: null };
  const back = Boolean(b.healed_at) && serverNow() >= Date.parse(b.healed_at);
  return { wounded: Number(b.wounded) || 0, healing: back ? 0 : Number(b.healing) || 0, healed_at: back ? null : b.healed_at };
};
// The squad a fight sends: the Section's headcount less its sickbay (Reviewer BP-12a-1 F3). With nobody fit and no heal
// affordable or under way, the wounded go anyway, so a pending raid can never strand the team.
const fitToFight = d => {
  const h = Math.min(8, d.section.headcount);
  const b = bayOf(d.section.section_id);
  const fit = Math.max(0, h - b.wounded - b.healing);
  if (fit > 0 || h === 0) return fit;
  return b.healing > 0 || credits >= HEAL_CREDITS * b.wounded ? 0 : h;
};
let adoptedFor = null;  // the campaign whose ops body has been adopted (the Company screen needs one, BP-12b)
let companyAsk = null;  // the heal or replace in flight: { action, target }
let companyNotice = null; // its refusal's line on the card: { action, target, kind }
let fightDress = null;  // the demo fight's own briefing dressing, { name, lead }, re-applied on every show (Reviewer BP-11b-2 F2)
let fitNote = null;     // the briefing's 0-fit line
let bankNote = null;    // the one-off "ore banked" line after a laden leg lands
let notice = '';        // a one-off refusal line (the Crew's), until the next adopted body
let seenBattle = null;  // the raid or ambush whose briefing has opened once ('raid:<site>', 'ambush:<site>')
let legKey = null;      // the site and leg the two reads below belong to
let midAsked = false;   // the pulled haul's midpoint read has been sent
let landAsked = false;  // the laden leg's arrival read has been sent
let throttleUntil = 0;  // a 429's Retry-After, in browser ms: no call and no button until then
let throttleSecs = 0;
let hubWasShown = false;
let base = {};          // the server's base: which buildings stand
let baseOpen = false;   // the base sheet is showing
let justBuilt = null;   // the building the last adopted POST put up, for its moment on the sheet
let titles = new Map(); // site_id → { title, mission_id }, from every adopted board, kept for the campaign
let walk = null;        // { depart_at, q, r }: where the leg leaving at depart_at set out from (after a Next)
let nextFrom = null;    // the team's hex while an ops/next is in flight
let picking = false;    // Next mission: choosing the next site
let pickSite = null;    // the site chosen for Next
let confirming = false; // the ore-on-site confirm is showing
let wonCredits = 0;     // the pay banked by the contract fight the strip reports
let opFight = false;    // the fight on the briefing was opened from here (the AAR note's guard)
let reopen = null;      // the opener that showed the demo fight's briefing: fight or fightContract (Reviewer BP-12b F2)
let fitTimer = null;    // the 0-fit briefing's re-dress at healed_at (Reviewer BP-12b F2)
let refreshDue = 0;     // when the one refresh read after a contract latch is due, in browser ms
let fightHead = null; // the first Section's headcount when the demo opened the fight (Reviewer BP-11b-1 F1)
let endOnHub = null;   // a Section-at-0 end of run waiting for the hub, so it never cuts an AAR (Reviewer BP-11b-1 F1)
let listOpen = false;   // the contracts list is showing

let strip = null, titleNode = null, lineNodes = [], actions = null, buttons = [];
let dig = null, bankChip = null, endNote = null, baseButton = null, baseSheet = null;
let list = null;
const handlers = [null, null, null];

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
// Next: an outbound, arrived op with no battle pending; a contract only once fought (Reviewer BP-11a-4 F1/F2).
const canNext = o => Boolean(o && o.leg === 'outbound' && progress(o) >= 1 && !battleOf(o) && (o.kind === 'dig' || o.fought));
const hexDistance = (a, b) => (Math.abs(a.q - b.q) + Math.abs(a.r - b.r) + Math.abs(a.q + a.r - b.q - b.r)) / 2;
const sameHex = (a, b) => a.q === b.q && a.r === b.r;

function titleOf(site) {
  if (!site) return COPY.digTitleOther;
  if (site.kind === 'contract') return site.mission_id === RELIC.mission_id ? RELIC.name : (site.name || site.mission_id || '');
  return COPY.digTitle[site.tier] || COPY.digTitleOther;
}

// The op's title from the cache, never the board (Reviewer BP-11a-3 F2).
function opTitle(o) {
  const t = titles.get(o.site_id);
  if (t) return t.title;
  return o.site_id.startsWith('contract:') ? COPY.contractSite : COPY.digTitleOther;
}

// Where the op's leg runs from and to: outbound from the hex the team left (the gateway unless a
// Next set out from elsewhere), homeward from the site to the gateway.
function legEnds(o) {
  if (o.leg !== 'outbound') return [o.at, gateway];
  return [walk && walk.depart_at === o.depart_at ? walk : gateway, o.at];
}

function saveWalk(departAt, from) {
  walk = { depart_at: departAt, q: from.q, r: from.r };
  try { sessionStorage.setItem(walkKey(campaignId), JSON.stringify(walk)); } catch { /* memory only */ }
}

function loadWalk(id) {
  try {
    const w = JSON.parse(sessionStorage.getItem(walkKey(id)) || 'null');
    return w && typeof w.depart_at === 'string' ? w : null;
  } catch {
    return null;
  }
}

// ── the strip ────────────────────────────────────────────────────────────────
function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

// `lines`: up to three strings; `acts`: up to three [text, onClick, disabled] buttons.
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
  renderCompany(); // the Company card's buttons follow held() and the last body (BP-12b, H3)
  // While a raid or an ambush waits, the hub's own GUARD_DUTY would count as its fight
  // (Reviewer BP-8b-2 F1), so the mission list is shut until the dig's fight is fought.
  deps.$('btn-missions').disabled = Boolean(battleOf(op));
  bankChip.textContent = bankedLine(banked, credits);
  bankChip.hidden = !(banked > 0 || credits > 0);
  shownPhase = null;
  shownSecs = null;
  const rate = Date.now() < throttleUntil ? fill(DIG_COPY.rate, { n: throttleSecs }) : notice;
  baseButton.hidden = !gateway;
  if (baseOpen || listOpen) {
    // A sheet covers the strip; the phase is noted so a leg's frames don't redraw it each time.
    hideStrip();
    hideDig(dig);
    if (op) shownPhase = `${op.leg}:${progress(op) >= 1}`;
    if (op && progress(op) < 1) shownSecs = secsLeft(op);
    if (baseOpen) {
      list.node.hidden = true;
      renderBase(baseSheet, { base, banked, credits, out: Boolean(op), held: held(), rate }, justBuilt);
      justBuilt = null;
    } else {
      hideBase(baseSheet);
      renderList();
    }
    return;
  }
  hideBase(baseSheet);
  list.node.hidden = true;
  if (op && picking && canNext(op)) {
    hideDig(dig);
    shownPhase = `${op.leg}:true`;
    return showPick(rate);
  }
  if (op && op.leg === 'outbound' && op.dig && progress(op) >= 1) return showDig(op);
  hideDig(dig);
  if (op) {
    const title = opTitle(op);
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
    if (op.kind === 'contract') {
      // Fight while unfought; Next mission wherever canNext allows it; Head home always.
      const acts = [];
      if (!op.fought) acts.push([COPY.fight, fightContract]);
      if (canNext(op)) acts.push([COPY.next, startPick]);
      acts.push([DIG_COPY.home, sendReturn]);
      const line = !op.fought ? fill(COPY.atContract, { title })
        : op.fought === 'won' ? fill(wonCredits > 0 ? COPY.won : COPY.wonNoPay, { title, credits: wonCredits }) : fill(COPY.lost, { title });
      return setStrip('', [line, rate], acts);
    }
    return setStrip('', [fill(COPY.onSite, { title }), rate], [[COPY.ret, sendReturn]]);
  }
  if (selected) return showSheet(selected);
  if (bankNote) return setStrip('', [bankNote, rate], [[BASE_COPY.button, openBase], [COPY.close, close]]);
  if (noDigOpen) return setStrip('', [COPY.noDig, rate], [[COPY.close, close]]);
  if (rate) return setStrip('', [rate], notice ? [[COPY.close, close]] : []);
  hideStrip();
}

// The Hold-or-Fold screen: the panel stays up for as long as the team is on site, so heat and ore
// are never behind a tap. With ore on site the only way home is Pull out: walking away would
// throw the pile away, so that button is offered only at 0 ore. Next mission joins them once the
// dig can be left for another site.
function showDig(o) {
  const title = opTitle(o);
  const rate = Date.now() < throttleUntil ? fill(DIG_COPY.rate, { n: throttleSecs }) : notice;
  shownPhase = `${o.leg}:true`;
  renderDig(dig, o.dig, store.roster(campaignId));
  // Next mission joins wherever canNext allows it (never while raiders wait: canNext says so).
  const next = canNext(o) ? [[COPY.next, startPick]] : [];
  if (o.dig.raid) return setStrip(title, [fill(DIG_COPY.raidWait, { title }), rate], [[DIG_COPY.fight, fight], ...next]);
  const second = o.dig.ore_on_site > 0 ? [DIG_COPY.pull, sendPull] : [DIG_COPY.home, sendReturn];
  return setStrip(title, [rate], [[DIG_COPY.drill, sendDrill], second, ...next]);
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

// Next mission's three steps: the prompt, the chosen site's hop, and the ore-on-site confirm.
function showPick(rate) {
  if (!pickSite) return setStrip('', [COPY.pick, rate], [[COPY.cancel, cancelPick]]);
  if (confirming) {
    return setStrip('', [fill(COPY.leaveOre, { ore: op.dig.ore_on_site }), rate],
      [[COPY.leaveIt, () => sendNext(pickSite)], [COPY.stay, cancelPick]]);
  }
  const hop = hexDistance(op.at, pickSite);
  // Display only: the server's leg is the site's seconds per hex times the hop, at least 1 s.
  const secs = Math.max(1, Math.floor(pickSite.travel_secs / Math.max(1, pickSite.distance)) * hop);
  return setStrip(titleOf(pickSite), [fill(COPY.hop, { n: hop }), fill(COPY.hopSecs, { s: secs }), rate],
    [[COPY.go, goNext], [COPY.cancel, cancelPick]]);
}

function startPick() {
  picking = true;
  pickSite = null;
  confirming = false;
  refresh();
}

function cancelPick() {
  picking = false;
  pickSite = null;
  confirming = false;
  refresh();
}

// A site for Next: any board site but the one the team stands on (the server's "already there").
function choose(site) {
  if (!site || !op || sameHex(site, op.at)) return;
  pickSite = site;
  confirming = false;
  refresh();
}

// Leaving a dig with ore on site walks away from it (BP-11a-4), so that asks first.
function goNext() {
  if (!pickSite) return;
  if (op && op.dig && op.dig.ore_on_site > 0) {
    confirming = true;
    return refresh();
  }
  sendNext(pickSite);
}

function close() {
  selected = null;
  noDigOpen = false;
  bankNote = null;
  notice = '';
  hideStrip();
}

// The Base button toggles the sheet; the bank note's Base opens it.
function openBase() {
  closeLegend();
  baseOpen = true;
  listOpen = false;
  selected = null;
  noDigOpen = false;
  bankNote = null;
  refresh();
}

function closeBase() {
  baseOpen = false;
  refresh();
}

// A sheet and the legend share the box over the bar, so only one is open at a time (Reviewer BP-10b F2).
function closeLegend() {
  deps.$('legend').hidden = true;
  deps.$('btn-legend').setAttribute('aria-expanded', 'false');
}

// ── the contracts list (A2) ──────────────────────────────────────────────────
// Missions opens the board's contracts, not the keeper list: a row hands off to its pin, so every
// contract is walked to and fought on site.
function openList() {
  closeLegend();
  listOpen = true;
  baseOpen = false;
  noDigOpen = false;
  bankNote = null;
  refresh();
}

function closeList() {
  listOpen = false;
  refresh();
}

// Both sheets shut with one refresh (Reviewer BP-11b-2 F3).
function closeSheets() {
  baseOpen = false;
  listOpen = false;
  refresh();
}

function renderList() {
  const out = Boolean(op) && !picking;
  list.sub.textContent = out ? COPY.listOut : COPY.listSub;
  list.rows.replaceChildren();
  for (const site of sites.filter(s => s.kind === 'contract')) {
    const row = el('button', 'btn ops-list-row');
    row.type = 'button';
    row.disabled = out;
    row.append(el('span', 'ops-list-name', titleOf(site)),
      el('span', 'ops-list-meta', fill(COPY.listMeta, { n: Math.round(site.distance), s: Math.round(site.travel_secs) })));
    row.addEventListener('click', () => listPick(site));
    const item = el('li', null);
    item.append(row);
    list.rows.append(item);
  }
  list.node.hidden = false;
}

function listPick(site) {
  listOpen = false;
  centre(site);
  if (op) {
    if (picking) return choose(site);
    return refresh();
  }
  selected = site;
  noDigOpen = false;
  bankNote = null;
  refresh();
}

// Pans the map so the site sits in the middle of the canvas.
function centre(site) {
  if (!map) return;
  const [sx, sy] = map.hexToScreen(site.q, site.r);
  map.view.cx += (sx - map.w / 2) / map.view.s;
  map.view.cy += (sy - map.h / 2) / map.view.s;
  map.clamp();
  map.requestFrame();
}

// ── fights ───────────────────────────────────────────────────────────────────
// The raid's or the ambush's GUARD_DUTY briefing, with the dig's lead line over the mission's
// own text. The Commander is the campaign's stored one, the one the dig start picked.
async function fight() {
  const b = battleOf(op);
  if (!b) return;
  const lead = op.dig.raid
    ? fill(DIG_COPY.raidLead, { title: opTitle(op) })
    : fill(DIG_COPY.ambushLead, { carried: op.dig.carried });
  opFight = true;
  reopen = fight;
  fightHead = sectionHead();
  fightDress = { name: null, lead };
  await deps.openMission(b.mission_id, () => deps.enterHub());
  if (!deps.$('screen-briefing').hidden) dressFight();
}

// The contract's own briefing; its mission comes from the title cache, never the board. The relic
// contract's name and text are the demo's (A12); every briefing gets the stress and trait lines.
async function fightContract() {
  const t = op && titles.get(op.site_id);
  if (!t || !t.mission_id) return;
  opFight = true;
  reopen = fightContract;
  fightHead = sectionHead();
  fightDress = t.mission_id === RELIC.mission_id ? { name: RELIC.name, lead: RELIC.lead } : { name: null, lead: null };
  await deps.openMission(t.mission_id, () => deps.enterHub());
  if (!deps.$('screen-briefing').hidden) dressFight();
}

function clearFitTimer() { if (fitTimer !== null) clearTimeout(fitTimer); fitTimer = null; }

// F2: at healed_at the briefing is opened again by its own opener (openMission's cache, no network),
// so the keeper redraws the squad line and Deploy, then dressFight runs on the new state.
function redress() {
  fitTimer = null;
  if (opFight && reopen && !deps.$('screen-briefing').hidden) reopen();
}

// The demo fight's dressing over the keeper's briefing: the stress and trait lines, the fight's
// own name and lead, and the 0-fit lines. Run after the fight opens it and on every later show
// (a deploy 429's Back, openMission's static-GET retry: Reviewer BP-11b-2 F2), so nothing is lost.
function dressFight() {
  clearFitTimer();
  dressBriefing();
  if (fightDress && fightDress.name) deps.$('briefing-name').textContent = fightDress.name;
  if (fightDress && fightDress.lead) deps.$('briefing-text').textContent = fightDress.lead;
  const roster = deps.getRoster();
  const section = roster && Array.isArray(roster.sections) ? roster.sections[0] : null;
  let text = '';
  if (section) {
    const d = { section };
    const squad = fitToFight(d);
    const b = bayOf(section.section_id);
    if (squad === 0 && section.headcount > 0) {
      deps.$('btn-deploy').disabled = true; // after the keeper's disabled = false (showBriefing runs before show)
      text = b.healing > 0 ? COPY.zeroFitHealing : COPY.zeroFit;
      if (b.healing > 0 && b.healed_at) fitTimer = setTimeout(redress, Math.max(0, Date.parse(b.healed_at) - serverNow()) + 100);
    } else if (squad === Math.min(8, section.headcount) && b.wounded > 0 && b.healing === 0) {
      text = COPY.walkingWounded;
    }
  }
  if (!fitNote) {
    fitNote = el('p', 'briefing-fit');
    deps.$('briefing-squad').after(fitNote);
  }
  fitNote.textContent = text;
  fitNote.hidden = !text;
}

// The campaign Commander's name from main.js's roster, for the card's opening line.
function commanderName() {
  const camp = store.campaign();
  const roster = deps.getRoster();
  const c = camp && roster && Array.isArray(roster.commanders) ? roster.commanders.find(x => x.commander_id === camp.commander_id) : null;
  return c && typeof c.name === 'string' ? c.name : '';
}

// The first Section's headcount in main.js's roster, the one battle.js's squad reads.
function sectionHead() {
  const roster = deps.getRoster();
  const section = roster && Array.isArray(roster.sections) ? roster.sections[0] : null;
  return section ? section.headcount : null;
}

// The AAR's loss lines for a fight opened from here: who died, and the Section's count after it
// (the server takes one soldier per KIA or MIA when the fight latches).
function aarNote({ report, section }) {
  if (!opFight) return [];
  opFight = false;
  const before = fightHead;
  fightHead = null;
  const dead = (report && report.unit_results || []).filter(u => u.final_status === 'KIA' || u.final_status === 'MIA');
  const lines = [dead.length
    ? fill(COPY.aarLost, { names: dead.map(u => fill(COPY.aarDead, { name: u.unit_name, status: u.final_status })).join(', ') })
    : COPY.aarNone];
  // The count before the fight is the one taken when it was opened: a read that latched the fight
  // in the meantime would already have lowered the roster's (Reviewer BP-11b-1 F1).
  const a = before ?? (section && section.headcount);
  if (section) lines.push(fill(COPY.aarSection, { name: section.name, a, b: Math.max(0, a - dead.length) }));
  // BP-12b: the wounded go to the sickbay when the fight latches.
  const wounded = (report && report.unit_results || []).filter(u => u.final_status === 'Wounded').length;
  if (wounded > 0) lines.push(fill(COPY.aarWounded, { n: wounded }));
  return lines;
}

// The run is over: nobody is left to send. The keeper end screen's New run does the rest.
function endOfRun(kind) {
  const c = END[kind];
  const roster = store.roster(campaignId);
  const section = roster && Array.isArray(roster.sections) ? roster.sections[0] : null;
  deps.$('end-title').textContent = c.title;
  deps.$('end-name').textContent = kind === 'soldiers' && section ? section.name : '';
  deps.$('end-grade').textContent = '';
  deps.$('end-flaw').textContent = '';
  deps.$('end-body').textContent = c.body;
  deps.$('end-glorious').textContent = '';
  deps.$('end-glorious').hidden = true;
  deps.$('btn-new-run').disabled = false;
  deps.show('screen-end');
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
  const ends = op && gateway ? legEnds(op) : null;
  if (ends) {
    const [gx, gy] = m.hexToScreen(ends[0].q, ends[0].r);
    const [sx, sy] = m.hexToScreen(ends[1].q, ends[1].r);
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
      // Each flag is set only when its read is sent: a read skipped while another call is in flight
      // is sent from the frame after that call clears (Reviewer BP-9b F1).
      if (home && !landAsked && !held()) { landAsked = true; load(); } // a laden leg landed: that read banks the ore
      if (!ambush && op.leg === 'homeward' && op.dig && op.dig.ended_by === 'pulled' && p >= 0.5 && !midAsked && !held()) {
        midAsked = true; // an ambush on this haul reveals on the first read at or after the midpoint
        load();
      }
      if (ends) {
        const [from, to] = ends;
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
  if (baseOpen || listOpen) return; // a sheet covers the map
  if (op) { // the strip stays while the team is out; Next mission's pick takes a pin
    if (picking && !confirming) choose(pick(x, y));
    return;
  }
  selected = pick(x, y);
  noDigOpen = false;
  bankNote = null;
  notice = '';
  if (selected) showSheet(selected);
  else refresh();
}

// ── calls ────────────────────────────────────────────────────────────────────
// `built`: the building id when this body answers that building's POST, else undefined.
// `spent`: what this body's own request cost from the bank (a heal or a replace), so a contract it
// latches still reports the full pay (BP-12b, H4).
function adopt(body, built, spent = 0) {
  if (!body || body.campaign_id !== campaignId) return;
  company = body.company || { sections: [], trophies: [] };
  adoptedFor = campaignId;
  offset = Date.parse(body.now) - Date.now();
  sites = Array.isArray(body.sites) ? body.sites : [];
  for (const s of sites) titles.set(s.site_id, { title: titleOf(s), mission_id: s.mission_id || null });
  gateway = body.gateway || null;
  const before = op;
  const was = banked;
  const wasCredits = credits;
  op = body.active_op || null;
  banked = Number(body.ore_banked) || 0;
  credits = Number(body.credits_banked) || 0;
  base = body.base || {};
  justBuilt = built && base[built] === true && baseOpen ? built : null; // a moment only for a sheet still open when the build lands (Reviewer BP-10b F1)
  notice = '';
  if (op) selected = null;
  else if (selected) selected = siteOf(selected.site_id); // the board may have moved on (Reviewer BP-9a F4)
  if (!canNext(op)) { picking = false; pickSite = null; confirming = false; }
  else if (pickSite) pickSite = siteOf(pickSite.site_id);
  const key = op ? `${op.site_id}:${op.leg}:${op.depart_at}` : null;
  if (key !== legKey) { legKey = key; midAsked = false; landAsked = false; }
  if (op) bankNote = null;
  else if (banked > was && before && before.leg === 'homeward' && before.dig && before.dig.carried > 0) {
    bankNote = fill(DIG_COPY.banked, { gained: banked - was });
  }
  if (!op && gateway && !noDigOffered && !sites.some(st => st.kind === 'dig')) {
    noDigOffered = true;
    noDigOpen = true;
  }
  // A fight latched on this read: a raid's or an ambush's key went away, or the contract the team
  // stood at unfought is now fought (or wiped out, which clears the op).
  const raidLatched = Boolean(battleKey(before) && battleKey(before) !== battleKey(op));
  const contractLatched = Boolean(before && before.kind === 'contract' && before.leg === 'outbound' && !before.fought && (!op || op.fought));
  if (contractLatched) wonCredits = Math.max(0, credits - wasCredits + spent); // a heal or replace body that latches pays for itself first (H4)
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
  if (raidLatched || contractLatched) rereadRoster();
  if (contractLatched) armRefresh();
  if (relicWon(op) && hubShown() && !cardSeen(campaignId)) showCard({ commander: commanderName() }, () => deps.enterHub()); // A12: DEMO-001 after a relic win, on the hub only
  if (opening) fight();
}

// The stored roster is read again (a meltdown cost Crew heads; a fight cost soldiers), so the Crew
// line, a reload's reveal and the next squad (main.js's roster, through deps.setRoster) all show
// the new headcount. A Section at 0 ends the run. Under the roster limiters, not the ops ones.
async function rereadRoster() {
  const id = campaignId;
  try {
    const body = await getRoster(id);
    if (id !== campaignId) return;
    store.setRoster(id, body);
    deps.setRoster(body);
    const section = Array.isArray(body.sections) ? body.sections[0] : null;
    if (section && section.headcount === 0) {
      if (hubShown()) return endOfRun('soldiers');
      endOnHub = 'soldiers'; // off the hub (an AAR may be showing): the end of run waits for it
      return;
    }
    refresh();
  } catch { /* the line keeps the old count; the next roster load corrects it */ }
}

// One board read after a contract's fight, once the server has put it back on the board (A3).
function armRefresh() {
  refreshDue = Date.now() + REFRESH_READ_MS;
  setTimeout(refreshTick, REFRESH_READ_MS);
}

// A held moment (another call in flight, or a 429) retries on the next tick.
function refreshTick() {
  if (!refreshDue) return;
  const wait = refreshDue - Date.now();
  if (wait > 0) { setTimeout(refreshTick, wait); return; }
  if (held() || !campaignId || !hubShown()) { setTimeout(refreshTick, 500); return; } // the read goes out only on the hub (Reviewer BP-11b-1 F1)
  refreshDue = 0; load();
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
  setTimeout(() => { throttleUntil = 0; refresh(); if (hubShown()) load(); else { if (!refreshDue) refreshDue = Date.now(); refreshTick(); } }, throttleSecs * 1000); // off the hub, the read waits for it; a pending refresh keeps its time (Reviewer BP-11b-1 rework F1)
  return true;
}

async function load() {
  const id = campaignId;
  if (!id || held()) return; // one read at a time (Reviewer BP-9a F3)
  if (refreshDue && refreshDue <= Date.now()) refreshDue = 0; // a read after the due time is the refresh (Reviewer BP-11b-1 rework F2)
  busy = true;
  refresh();
  let body = null, err = null;
  try {
    body = (await request('GET', opsPath(id))).body;
  } catch (e) {
    err = e;
  }
  busy = false;
  if (map) map.requestFrame(); // a landing or midpoint read skipped meanwhile goes out from this frame
  if (id !== campaignId) return;
  if (!err) return adopt(body);
  refresh();
  if (stale(err) || rated(err)) return;
  if (err instanceof ApiError && (err.kind === 'notfound' || err.kind === 'forbidden')) return deps.staleCampaign();
  deps.handleError(err, retry);
}

// One POST; a refusal other than a stale campaign re-reads the board once and shows its state.
// `built`: the building a build POST asks for, so its success gets the sheet's moment.
// `spentOf(res)`: what a heal or replace spent, from its own body (H4). `beforeAdopt(res)`: run
// on a 200 before the body is adopted (replace patches the kept roster there, BP-12b).
async function send(path, body, built, spentOf, beforeAdopt) {
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
  const from = nextFrom;
  nextFrom = null;
  if (map) map.requestFrame();
  if (id !== campaignId) return;
  const ask = companyAsk;
  companyAsk = null;
  if (!err) {
    if (from && res && res.active_op) saveWalk(res.active_op.depart_at, from);
    if (beforeAdopt) beforeAdopt(res);
    return adopt(res, built, spentOf ? spentOf(res) : 0);
  }
  refresh();
  if (stale(err) || rated(err)) return;
  if (err instanceof ApiError && err.kind === 'conflict' && ask) {
    // BP-12b: heal's and replace's refusals by exact text; the bank comes from the re-read, never from here.
    if (err.message === REFUSAL.short || err.message === REFUSAL.out) {
      companyNotice = { ...ask, kind: err.message === REFUSAL.short ? 'short' : 'out' };
      return load();
    }
    if (err.message === REFUSAL.sectionFull || err.message === REFUSAL.crewFull) {
      companyNotice = { ...ask, kind: 'full' };
      refresh();
      return rereadRoster(); // our ids only, so "no room" means the kept count is stale
    }
  }
  if (err instanceof ApiError && err.kind === 'conflict') {
    // Nobody left to send ends the run; no Crew only shuts the digs.
    if (err.message === NO_SOLDIERS) return endOfRun('soldiers');
    if (err.message === NO_COMMANDER) return endOfRun('commander');
    if (err.message === NO_CREW) {
      selected = null;
      picking = false;
      pickSite = null;
      confirming = false;
      notice = COPY.noCrew;
      return refresh();
    }
  }
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

function sendBuild(id) {
  send(opsPath(campaignId) + '/build', { building: id }, id);
}

// That Section's healing in a body's company, absent = 0: what a heal spent, since the server heals
// everyone wounded after its own latch (H4).
const healingIn = (res, id) => {
  const s = res && res.company && Array.isArray(res.company.sections) ? res.company.sections.find(x => x.section_id === id) : null;
  return s ? Number(s.healing) || 0 : 0;
};

// Heal a Section's wounded (BP-12b): the card's Patch up.
function sendHeal(sectionId) {
  if (held() || !campaignId) return;
  companyNotice = null;
  companyAsk = { action: 'heal', target: 'section' };
  send(opsPath(campaignId) + '/heal', { section_id: sectionId }, false, res => HEAL_CREDITS * healingIn(res, sectionId));
}

// The kept roster after a replace's 200: the row the server added to gains `count` (never past
// 8). Done before the body is adopted, so a roster re-read that the body starts (a latch) lands
// after it and wins.
function patchHeads(target, id, count) {
  const r = deps.getRoster();
  if (!r) return;
  const rows = target === 'section' ? r.sections : r.crews;
  const key = target === 'section' ? 'section_id' : 'crew_id';
  const row = Array.isArray(rows) ? rows.find(x => x[key] === id) : null;
  if (!row) return;
  row.headcount = Math.min(8, (Number(row.headcount) || 0) + count);
  store.setRoster(campaignId, r);
  deps.setRoster(r);
}

// Replace (BP-12b): `count` heads on one of the player's own rows, `each` credits a head.
function sendReplace(target, id, count, each) {
  if (held() || !campaignId) return;
  companyNotice = null;
  companyAsk = { action: 'replace', target };
  const body = target === 'section' ? { section_id: id, count } : { crew_id: id, count };
  send(opsPath(campaignId) + '/replace', body, false, () => count * each, () => patchHeads(target, id, count));
}

// The team leaves the hex it stands on (cached as the next leg's origin when the move lands).
function sendNext(site) {
  if (!site || !op || held() || !canNext(op)) return;
  nextFrom = { q: op.at.q, r: op.at.r };
  send(opsPath(campaignId) + '/next', { site_id: site.site_id });
}

// ── the hub's re-entry and the end screen ─────────────────────────────────────
// Both are seen through their `hidden` attribute, which main.js's show() sets, so no keeper code
// calls in here. Back on the hub with an op out, one read latches a fought raid, ambush or
// contract (the fight's AAR returns to the hub without one) and shows anything that moved while away.
function watch() {
  const hub = deps.$('screen-hub'), end = deps.$('screen-end');
  hubWasShown = !hub.hidden;
  new MutationObserver(() => {
    const shown = !hub.hidden;
    if (shown && !hubWasShown && endOnHub) { // a Section lost while away ends the run now, before any read
      const kind = endOnHub;
      endOnHub = null;
      hubWasShown = shown;
      return endOfRun(kind);
    }
    if (shown && !hubWasShown && op) load(); // a contract latches on this read (Reviewer BP-11a-2 CF1); a laden haul still reads (BP-10b F3)
    hubWasShown = shown;
  }).observe(hub, { attributes: true, attributeFilter: ['hidden'] });
  new MutationObserver(() => { if (!end.hidden) writeEndNote(); }).observe(end, { attributes: true, attributeFilter: ['hidden'] });
  // Missions opens the board's contracts (A2): taken before the keeper list's own listener.
  hub.addEventListener('click', e => {
    if (!(e.target instanceof Element) || !e.target.closest('#btn-missions')) return;
    e.stopPropagation();
    e.preventDefault();
    openList();
  }, true);
  // The legend shuts any open sheet first; the keeper toggle then opens it (Reviewer BP-10b F2).
  hub.addEventListener('click', e => {
    if (!(e.target instanceof Element) || !e.target.closest('#btn-legend') || !(baseOpen || listOpen)) return;
    closeSheets();
  }, true);
  // Reviewer BP-11b-2 F2: every show of the briefing during a demo fight gets its dressing again
  // (the keeper re-renders it on a deploy 429's Back and on openMission's static-GET retry).
  const briefing = deps.$('screen-briefing');
  new MutationObserver(() => { if (briefing.hidden) clearFitTimer(); else if (opFight) dressFight(); }).observe(briefing, { attributes: true, attributeFilter: ['hidden'] });
}

// New run starts a new campaign, and ore, credits and buildings belong to the campaign: the end
// screen says what stays behind.
function writeEndNote() {
  const camp = store.campaign();
  let text = '';
  if (camp && camp.campaign_id === campaignId) {
    const built = Object.keys(base).some(k => base[k] === true);
    if (banked > 0 || credits > 0) text = built ? BASE_COPY.endBase : DIG_COPY.endBanked;
    else if (op && op.dig && op.dig.ore_on_site + op.dig.carried > 0) text = built ? BASE_COPY.endBaseOut : DIG_COPY.endOut;
    else if (built) text = BASE_COPY.endBaseOnly;
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
  credits = 0;
  company = { sections: [], trophies: [] };
  adoptedFor = null;
  companyAsk = null;
  companyNotice = null;
  bankNote = null;
  notice = '';
  seenBattle = null;
  legKey = null;
  midAsked = false;
  landAsked = false;
  base = {};
  baseOpen = false;
  justBuilt = null;
  titles = new Map();
  walk = campaignId ? loadWalk(campaignId) : null;
  nextFrom = null;
  picking = false;
  pickSite = null;
  confirming = false;
  wonCredits = 0;
  refreshDue = 0;
  fightHead = null;
  endOnHub = null;
  listOpen = false;
  refresh();
  m.overlay = draw;
  m.onTap = tap;
  load();
}

function createList() {
  const node = el('section', 'base-sheet ops-list');
  node.hidden = true;
  const head = el('div', 'base-head');
  const shut = el('button', 'btn base-close', COPY.close);
  shut.type = 'button';
  shut.addEventListener('click', closeList);
  head.append(el('h2', 'ops-title', COPY.listTitle), shut);
  const sub = el('p', 'base-note');
  const rows = el('ul', 'ops-list-rows');
  node.append(head, sub, rows);
  return { node, sub, rows };
}

// `d`: main.js's $, show, handleError, staleCampaign, enterHub, getRoster, setRoster and the battle's single briefing.
export function mountOps(d) {
  deps = d;
  const style = document.createElement('style');
  style.textContent = CSS + DIG_CSS + BASE_CSS;
  document.head.append(style);
  strip = el('div', 'ops-strip');
  strip.hidden = true;
  titleNode = el('h2', 'ops-title');
  dig = createDigPanel();
  lineNodes = [el('p', 'ops-line'), el('p', 'ops-line'), el('p', 'ops-line')];
  actions = el('div', 'ops-actions');
  buttons = [el('button', 'btn btn-primary'), el('button', 'btn'), el('button', 'btn')];
  buttons.forEach((b, i) => {
    b.type = 'button';
    b.addEventListener('click', () => { if (handlers[i]) handlers[i](); });
    actions.append(b);
  });
  strip.append(titleNode, dig.node, ...lineNodes, actions);
  bankChip = el('div', 'ops-banked');
  bankChip.hidden = true;
  baseButton = el('button', 'ops-base', BASE_COPY.button);
  baseButton.type = 'button';
  baseButton.hidden = true;
  baseButton.addEventListener('click', () => (baseOpen ? closeBase() : openBase()));
  baseSheet = createBase(sendBuild, closeBase);
  list = createList();
  // A13: today's checkout from the hub at any time (no ops call). Leaving it returns to the hub.
  const shopButton = el('button', 'ops-shop', COPY.shop);
  shopButton.type = 'button';
  shopButton.addEventListener('click', () => deps.offerCheckout(() => deps.enterHub()));
  deps.$('screen-hub').append(strip, bankChip, baseButton, shopButton, baseSheet.node, list.node);
  endNote = el('p', 'end-ops');
  endNote.hidden = true;
  deps.$('btn-new-run').before(endNote);
  setAarNote(aarNote);
  setSquadSize(fitToFight);
  setBeforeOffer(() => deps.enterHub()); // A13: no checkout after a mission in the demo; the Demo Shop button opens it
  // BP-12b: the Company screen's view of this file's state and requests.
  mountCompany(deps, {
    adopted: () => Boolean(campaignId) && adoptedFor === campaignId && Boolean(store.campaign()) && store.campaign().campaign_id === campaignId,
    credits: () => credits,
    out: () => Boolean(op),
    held,
    serverNow,
    bay: id => company.sections.find(s => s.section_id === id) || { wounded: 0, healing: 0, healed_at: null },
    bayOf,
    squadOf: section => fitToFight({ section }),
    trophies: () => company.trophies,
    notice: () => companyNotice,
    clearNotice: () => { companyNotice = null; },
    heal: sendHeal,
    replace: sendReplace,
  });
  watch();
  onMapCreated(attach);
}
