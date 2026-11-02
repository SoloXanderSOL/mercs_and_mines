// demo-only (BP-11b-2): the numbers given their meaning (Breakpoint §4e Tier A: A6, A7, A8, A10, A11).
// Never ported to dev: the port deletes this file.
//
// The keeper screens draw their own copy; this file rewrites it where the demo needs more, by
// watching the keeper's DOM (no keeper code calls in here). XP is shown against the next rank's
// threshold (pinned below, and by the harness from commander_gen.rs), stress against 100, a rank-up
// says what it really changes in this build, each trait gets a plain line, the hub's Back opens
// "Your Company" with a stats card for the Section and the Crew, and the combat screen's factor
// labels say where each number comes from. It reads the roster the client already holds and makes
// no network call. Every string goes in through textContent.

import { store } from './api.js';

const RANK_XP = [100, 400, 1100, 2600]; // commander_gen.rs:132 RANK_THRESHOLDS (ranks 2..=5); harness k3 holds its own copy

// A7: what a rank really changes in this build (the roll sees no Commander until B5).
const RANK_LINES = {
  2: 'Rank 2. Cleared to command 4 Sections. You have one. The paperwork is very impressive.',
  3: 'Rank 3. A death on the relic dig or the Black Site now goes down as glorious.',
  4: 'Rank 4. Cleared for 64 Sections. Command admires the optimism.',
  5: 'Rank 5. Top rank. Any death now goes down as glorious.',
};

// A8: one plain line per trait, keyed by the wire string. Character notes only: no trait acts on the demo's roll.
const TRAITS = {
  "Trench-Sweeper": "Clears trenches with a shotgun and a short temper.",
  "Armor-Adept": "Talks to tanks. The tanks, reportedly, listen.",
  "Night-Stalker": "Prefers to work at night, in an ash-storm, or ideally both.",
  "Cannoneer": "Believes most problems are solved by heavy artillery. Rarely proved wrong.",
  "Marksman's Eye": "Trusts a sniper rifle more than people. Fair.",
  "Breacher": "Loves a fortified door. Less keen on open fields.",
  "Quartermaster's Ghost": "Haunts the stores. Nothing leaves without a signature.",
  "Corpseman's Touch": "A field medic at heart. Carries more bandages than ammunition.",
  "Suppression Doctrine": "Keeps the enemy's heads down. Hitting them is a secondary concern.",
  "Veteran's Instinct": "Has seen worse, and mentions it often.",
  "Anti-Armor Adept": "Hates vehicles. Carries something for every one of them.",
  "Battlefield Engineer": "Can dig a fortification out of anything, given a shovel and an argument.",
  "Embezzler": "Good in a fight. Counts the payroll more often than strictly necessary.",
  "Bloodthirsty": "Treats the Retreat order as a suggestion.",
  "Cowardly": "Superb at logistics. Less superb once the armour comes off.",
  "Paranoid": "Always watching. Mostly the wrong direction.",
  "Cult of Bob": "Devoted to Bob. Finds lunch in bins and takes the scenic route.",
  "The Informant": "Someone else pays them a retainer. Nobody knows who. Nobody asks.",
  "Glory Hound": "Writes their own after-action reports, in the third person.",
  "The Gambler": "Puts a cut of every payout on the Colosseum tables. Sometimes it comes back.",
  "Chrome-Chaser": "Buys their own augmentations. Do not ask where from.",
  "Martinet": "Drills the Section to a mirror shine. Nobody is allowed to relax.",
  "Squeak-Touched": "Leaves offerings at the Hamster Shrines. The hamsters know things.",
  "Doomsayer": "Announces everyone's death at every muster. Occasionally right.",
};

// A11: the keeper's labels (battle.js:45) → where each factor comes from. Gear stays its own line (C1, C6 change it).
const SCORE_LABELS = {
  'Squad skill': 'Squad: standard-issue Rookies',
  'Squad size': 'Headcount: {n} Rookies',
  'Training': 'Training: 8 per Rookie',
  'Gear': 'Gear: standard issue',
  'Mission fit': 'Mission fit: drills pay off on digs only',
  'Difficulty': 'Difficulty: set by the contract',
};

const COMPANY_COPY = {
  hubButton: 'Your Company',
  heading: 'Your Company',
  xpNext: '{xp} / {next} to Rank {r}',
  xpTop: '{xp} · top rank',
  stress: '{n} / 100',
  stressNote: 'At 100 they crack. The run ends there.',
  traitsNote: 'On file. In this build, traits colour the dossier, not the fight.',
  briefingCommander: 'Commanding: {name} · Stress {n} / 100',
  aarStress: 'Stress {n} / 100 (+{d})',
  aarXpNext: '+{g} XP · {xp} / {next} to Rank {r}',
  aarXpTop: '+{g} XP · top rank',
  sectionRookies: 'Rookies: {n} / 8',
  sectionHow: 'Each Rookie adds 8 Training. Up to five, each extra one adds 3 Headcount.',
  sectionLoss: 'Losses come off this number. None come back.',
  crewTitle: 'Mining Crew',
  crewMiners: 'Miners: {n} / 8',
  crewMorale: 'Morale: {m} / 100. A meltdown costs miners and 10 morale.',
  crewForeman: 'Foreman: {name}, {spec}',
  close: 'Close',
};

const CSS = `
.company-trait { grid-column: 1 / -1; margin: -0.6cqw 0 0.6cqw 30cqw; font-size: 3.5cqw; line-height: 1.3; color: var(--muted); }
.company-note { margin: 0; font-size: 3.6cqw; line-height: 1.35; color: var(--muted); text-align: left; }
.briefing-traits[hidden] { display: none; }
.briefing-traits { margin: 0; font-size: 3.8cqw; line-height: 1.35; color: var(--muted); }
.briefing-traits span { display: block; }
.chip[data-company] { cursor: pointer; border-color: var(--accent); }
.chip[data-company]:focus-visible { outline: 0.5cqw solid var(--accent); outline-offset: 0.5cqw; }
.company-card[hidden] { display: none; }
.company-card { position: absolute; left: 5cqw; right: 5cqw; top: 30cqw; z-index: 5; padding: 4cqw; display: flex; flex-direction: column; gap: 2cqw; text-align: left; background: rgba(11, 11, 12, 0.97); border: 0.4cqw solid var(--accent); border-radius: 2cqw; }
.company-card-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 6cqw; line-height: 1.1; color: var(--accent); }
.company-card-line { margin: 0; font-size: 4.2cqw; line-height: 1.35; }
.company-card-line.muted { font-size: 3.8cqw; color: var(--muted); }
.company-card .btn { margin-top: 1.5cqw; }
`;

const fill = (text, values) => text.replace(/\{(\w+)\}/g, (m, k) => (k in values ? String(values[k]) : m));

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

let deps = null;
let fromHub = false;  // the reveal about to render was opened by the hub's Your Company
let card = null;      // the stats card: { node, title, lines }
let traitsLine = null; // the briefing's trait lines, created on the first briefing

// ── the roster the client holds ────────────────────────────────────────────────
function roster() {
  const r = deps.getRoster();
  if (r) return r;
  const camp = store.campaign();
  return camp ? store.roster(camp.campaign_id) : null;
}

function campaignCommander() {
  const camp = store.campaign();
  const r = roster();
  if (!camp || !r || !Array.isArray(r.commanders)) return null;
  return r.commanders.find(c => c.commander_id === camp.commander_id) || null;
}

function firstSection() {
  const r = roster();
  return r && Array.isArray(r.sections) ? r.sections[0] || null : null;
}

// The next threshold from the server's rank, never a rank computed here. Rank 5 is the top.
function nextXp(rank) {
  const r = Number(rank);
  return Number.isInteger(r) && r >= 1 ? RANK_XP[r - 1] : undefined;
}

function xpLine(xp, rank) {
  const next = nextXp(rank);
  return next === undefined ? fill(COMPANY_COPY.xpTop, { xp }) : fill(COMPANY_COPY.xpNext, { xp, next, r: Number(rank) + 1 });
}

// ── A6, A8, A10 on the Commander card ──────────────────────────────────────────
function statRow(info, label) {
  for (const row of info.querySelectorAll('.stat')) {
    const l = row.querySelector('.stat-label');
    if (l && l.textContent === label) return row;
  }
  return null;
}

function dressReveal(container) {
  const cardNode = container.querySelector('.commander-card');
  if (!cardNode || cardNode.dataset.company === '1') return;
  cardNode.dataset.company = '1';
  hideCard();
  if (fromHub) {
    fromHub = false;
    const heading = container.querySelector('.reveal-heading');
    if (heading) heading.textContent = COMPANY_COPY.heading;
  }
  const c = campaignCommander();
  const xpRow = statRow(cardNode, 'XP');
  if (c && xpRow) xpRow.querySelector('.stat-value').textContent = xpLine(c.xp, c.rank);
  const stressRow = statRow(cardNode, 'Stress');
  if (c && stressRow) {
    stressRow.querySelector('.stat-value').textContent = fill(COMPANY_COPY.stress, { n: c.stress });
    cardNode.after(el('p', 'company-note', COMPANY_COPY.stressNote));
  }
  let last = null;
  for (const label of ['Specialisation', 'Fatal Flaw']) {
    const row = statRow(cardNode, label);
    const value = row && row.querySelector('.stat-value').textContent;
    if (!row || !Object.prototype.hasOwnProperty.call(TRAITS, value)) continue;
    last = el('p', 'company-trait', TRAITS[value]);
    row.after(last);
  }
  if (last) last.after(el('p', 'company-trait', COMPANY_COPY.traitsNote));
  // A10: the Section and Crew chips open their stats card.
  const counts = { Section: 0, Crew: 0 };
  for (const chip of container.querySelectorAll('li.chip')) {
    const kind = chip.querySelector('.chip-kind');
    const k = kind && kind.textContent;
    if (k !== 'Section' && k !== 'Crew') continue;
    chip.dataset.company = k === 'Section' ? 'section' : 'crew';
    chip.dataset.index = String(counts[k]++);
    chip.setAttribute('role', 'button');
    chip.setAttribute('tabindex', '0');
  }
}

// ── A10: the stats card ────────────────────────────────────────────────────────
function createCard() {
  const node = el('section', 'company-card');
  node.hidden = true;
  const title = el('h2', 'company-card-title');
  const lines = el('div', 'company-card-lines');
  const close = el('button', 'btn', COMPANY_COPY.close);
  close.type = 'button';
  close.addEventListener('click', hideCard);
  node.append(title, lines, close);
  return { node, title, lines };
}

function hideCard() {
  if (card) card.node.hidden = true;
}

function openCard(kind, index) {
  const r = roster();
  if (!r) return;
  const rows = [];
  let title = '';
  if (kind === 'section') {
    const s = Array.isArray(r.sections) ? r.sections[index] : null;
    if (!s) return;
    title = s.name;
    rows.push([fill(COMPANY_COPY.sectionRookies, { n: s.headcount }), false], [COMPANY_COPY.sectionHow, true], [COMPANY_COPY.sectionLoss, true]);
  } else {
    const w = Array.isArray(r.crews) ? r.crews[index] : null;
    if (!w) return;
    title = COMPANY_COPY.crewTitle;
    rows.push([fill(COMPANY_COPY.crewMiners, { n: w.headcount }), false], [fill(COMPANY_COPY.crewMorale, { m: w.morale }), false]);
    const f = Array.isArray(r.foremen) ? r.foremen[0] : null;
    if (f) rows.push([fill(COMPANY_COPY.crewForeman, { name: f.name, spec: f.specialization }), true]);
  }
  card.title.textContent = title;
  card.lines.replaceChildren(...rows.map(([text, muted]) => el('p', muted ? 'company-card-line muted' : 'company-card-line', text)));
  card.node.hidden = false;
}

function chipAction(e) {
  if (!(e.target instanceof Element)) return;
  const chip = e.target.closest('li.chip[data-company]');
  if (!chip) return;
  if (e.type === 'keydown') {
    if (e.key !== 'Enter' && e.key !== ' ') return;
    e.preventDefault();
  }
  openCard(chip.dataset.company, Number(chip.dataset.index) || 0);
}

// ── A6, A7 on the AAR ──────────────────────────────────────────────────────────
function dressAar(box) {
  const c = campaignCommander();
  for (const span of box.querySelectorAll('.aar-stat')) {
    const t = span.textContent;
    let m = /^Stress (-?\d+) \(\+(-?\d+)\)$/.exec(t);
    if (m) { span.textContent = fill(COMPANY_COPY.aarStress, { n: m[1], d: m[2] }); continue; }
    m = /^\+(-?\d+) XP$/.exec(t);
    if (m && c) {
      const next = nextXp(c.rank);
      span.textContent = next === undefined
        ? fill(COMPANY_COPY.aarXpTop, { g: m[1] })
        : fill(COMPANY_COPY.aarXpNext, { g: m[1], xp: c.xp, next, r: Number(c.rank) + 1 });
      continue;
    }
    m = /^Promoted: rank (\d+) to (\d+)$/.exec(t);
    if (m) {
      const lines = [];
      for (let r = Number(m[1]) + 1; r <= Number(m[2]); r++) if (RANK_LINES[r]) lines.push(RANK_LINES[r]);
      if (lines.length) span.textContent = lines.join(' ');
    }
  }
}

// ── A11 on the combat screen ───────────────────────────────────────────────────
// Only the label's text changes: no row is hidden, moved or restyled, so the presentation runs as before.
function dressScore(lines) {
  const s = firstSection();
  const n = s ? Math.min(8, s.headcount) : 0;
  for (const label of lines.querySelectorAll('li.score-row .score-label')) {
    const key = label.textContent;
    if (Object.prototype.hasOwnProperty.call(SCORE_LABELS, key)) label.textContent = fill(SCORE_LABELS[key], { n });
  }
}

// ── the briefing (demo_ops.js calls this once the briefing shows) ──────────────
export function dressBriefing() {
  if (!deps) return;
  const c = campaignCommander();
  const line = deps.$('briefing-commander');
  if (c) line.textContent = fill(COMPANY_COPY.briefingCommander, { name: c.name, n: c.stress });
  if (!traitsLine) {
    traitsLine = el('p', 'briefing-traits');
    line.after(traitsLine);
  }
  const parts = [];
  for (const value of c ? [c.specialization, c.fatal_flaw] : []) {
    if (Object.prototype.hasOwnProperty.call(TRAITS, value)) parts.push(el('span', '', `${value}: ${TRAITS[value]}`));
  }
  traitsLine.replaceChildren(...parts);
  traitsLine.hidden = parts.length === 0;
}

// `d`: demo_ops.js's deps (main.js's $, getRoster, ...).
export function mountCompany(d) {
  deps = d;
  const style = document.createElement('style');
  style.textContent = CSS;
  document.head.append(style);
  deps.$('btn-back').textContent = COMPANY_COPY.hubButton;
  // Taken before the keeper's own listener renders the reveal (its observer runs straight after that listener).
  deps.$('screen-hub').addEventListener('click', e => {
    if (e.target instanceof Element && e.target.closest('#btn-back')) fromHub = true;
  }, true);
  const reveal = deps.$('reveal-content');
  card = createCard();
  deps.$('screen-reveal').append(card.node);
  new MutationObserver(() => dressReveal(reveal)).observe(reveal, { childList: true, subtree: true });
  reveal.addEventListener('click', chipAction);
  reveal.addEventListener('keydown', chipAction);
  const aar = deps.$('aar-commander');
  new MutationObserver(() => dressAar(aar)).observe(aar, { childList: true });
  const lines = deps.$('battle-lines');
  new MutationObserver(() => dressScore(lines)).observe(lines, { childList: true });
}
