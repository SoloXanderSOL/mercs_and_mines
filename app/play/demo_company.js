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
//
// BP-12b: "Your Company" is the Company screen. Once demo_ops.js has adopted an ops body for the
// campaign, the Section card shows its bunks, its sickbay, Heal and Replace, the Crew card shows
// Replace, and a Trophy Shelf chip lists the recovered loot. The state and every request are
// demo_ops.js's, read through the view it registers; this file renders, keeps the costs and the
// shelf's lines, and ticks the heal countdown (no network call) while the card is open.

import { store } from './api.js';

const RANK_XP = [100, 400, 1100, 2600]; // commander_gen.rs:132 RANK_THRESHOLDS (ranks 2..=5); harness k3 holds its own copy
export const HEAL_CREDITS = 40;           // demo_ops.rs DEMO_HEAL_CREDITS; harness y14 holds its own copy
export const HEAL_SECS = 15;              // demo_ops.rs DEMO_HEAL_SECS (copy only: the body's healed_at decides)
export const BENCHES_HEAL_CREDITS = 20;   // demo_ops.rs DEMO_BENCHES_HEAL_CREDITS (BP-16a)
export const BENCHES_HEAL_SECS = 8;       // demo_ops.rs DEMO_BENCHES_HEAL_SECS (copy only: the body's healed_at decides)
// The Patch-up price for a base, by the server's rule (OpsBase::heal_credits / heal_secs, BP-16a): the Benches halve it.
export function healPrice(base) {
  return base && base.hobbyist_benches ? { each: BENCHES_HEAL_CREDITS, secs: BENCHES_HEAL_SECS } : { each: HEAL_CREDITS, secs: HEAL_SECS };
}
export const REPLACE_MERC_CREDITS = 120;  // demo_ops.rs DEMO_REPLACE_MERC_CREDITS
export const REPLACE_MINER_CREDITS = 80;  // demo_ops.rs DEMO_REPLACE_MINER_CREDITS

// A7: what a rank really changes in this build (since B5, each rank above 1 is also +2 on every roll).
const RANK_LINES = {
  2: 'Rank 2. +2 on every roll. Cleared to command 4 Sections. You have one. The paperwork is very impressive.',
  3: 'Rank 3. +4 on every roll. A death on the relic dig or the Black Site now goes down as glorious.',
  4: 'Rank 4. +6 on every roll. Cleared for 64 Sections. Command admires the optimism.',
  5: 'Rank 5. Top rank, +8 on every roll. Any death now goes down as glorious.',
};

// A8: one plain line per trait, keyed by the wire string. Since B5 every trait moves the roll (demo_roll.rs), and
// each line says where in words; the battle screen shows the server's points.
const TRAITS = {
  "Trench-Sweeper": "Clears trenches with a shotgun and a short temper. Suits the Supply Raid and the Black Site.",
  "Armor-Adept": "Talks to tanks. The tanks, reportedly, listen. Suits Guard Duty and holding the dig.",
  "Night-Stalker": "Prefers to work at night, in an ash-storm, or ideally both. Suits the relic dig.",
  "Cannoneer": "Believes most problems are solved by heavy artillery. Suits Guard Duty and holding the dig.",
  "Marksman's Eye": "Trusts a sniper rifle more than people. Fair. Suits Guard Duty and holding the dig.",
  "Breacher": "Loves a fortified door. Suits the Supply Raid and the Black Site.",
  "Quartermaster's Ghost": "Haunts the stores. Nothing leaves without a signature. Suits the relic dig.",
  "Corpseman's Touch": "A field medic at heart. Carries more bandages than ammunition. Suits the relic dig.",
  "Suppression Doctrine": "Keeps the enemy's heads down. Suits the Supply Raid and the Black Site.",
  "Veteran's Instinct": "Has seen worse, and mentions it often. Suits the relic dig.",
  "Anti-Armor Adept": "Hates vehicles. Carries something for every one of them. Suits the Supply Raid and the Black Site.",
  "Battlefield Engineer": "Can dig a fortification out of anything. Suits Guard Duty and holding the dig.",
  "Embezzler": "Sold the good ammo. Every fight starts a little behind.",
  "Bloodthirsty": "Treats the Retreat order as a suggestion. Better on an Assault, worse dug in.",
  "Cowardly": "Superb at logistics. Better dug in, worse on an Assault.",
  "Paranoid": "Always watching. Better dug in, and refuses to split the squad for a Flank.",
  "Cult of Bob": "Devoted to Bob. Stops to bless the rifles before every fight.",
  "The Informant": "Someone else pays them a retainer. The other side always seems to know we're coming.",
  "Glory Hound": "Writes their own after-action reports. Shines on hard contracts, coasts on easy ones.",
  "The Gambler": "Bets on every call. A good call pays double. So does a bad one.",
  "Chrome-Chaser": "Buys their own augmentations. Do not ask where from. They help, mostly.",
  "Martinet": "Drills the Section to a mirror shine. Better dug in, worse on a Flank.",
  "Squeak-Touched": "Leaves offerings at the Hamster Shrines. The hamsters send word before every fight.",
  "Doomsayer": "Announces everyone's death at every muster. The Section believes it, a little.",
};

// A11: the keeper's labels (battle.js:45) → where each factor comes from. The server's modifier rows (BP-14a) keep
// their own labels and pass through untouched. Gear stays its own line (C1, C6 change it).
const SCORE_LABELS = {
  'Squad skill': 'Squad: standard-issue Rookies',
  'Squad size': 'Headcount: {n} Rookies',
  'Training': 'Training: 8 per Rookie',
  'Gear': 'Gear: standard issue',
  'Mission fit': 'Mission type: drills pay off on digs only',
  'Difficulty': 'Difficulty: set by the contract',
};
const GUNSMITH_ROW = 'Gunsmith (your base)'; // demo_roll.rs's row label, shown as sent (Reviewer BP-16a CF (d))
const GEAR_HINT = 'Gear: standard. Gunsmith adds 10.'; // A11 (BP-16b): only when this fight's roll had no Gunsmith row

const COMPANY_COPY = {
  hubButton: 'Your Company',
  heading: 'Your Company',
  xpNext: '{xp} / {next} to Rank {r}',
  xpTop: '{xp} · top rank',
  stress: '{n} / 100',
  stressNote: 'At 100 they crack. The run ends there.',
  traitsNote: 'On file. Every trait moves the odds in a fight. The battle screen shows by how much.',
  briefingCommander: 'Commanding: {name} · Stress {n} / 100',
  aarStress: 'Stress {n} / 100 (+{d})',
  aarXpNext: '+{g} XP · {xp} / {next} to Rank {r}',
  aarXpTop: '+{g} XP · top rank',
  sectionRookies: 'Rookies: {n} / 8',
  sectionHow: 'Each Rookie adds 8 Training. Up to five, each extra one adds 3 Headcount.',
  sectionLoss: 'Losses come off this number. Replacements cost credits.',
  crewTitle: 'Mining Crew',
  crewMiners: 'Miners: {n} / 8',
  crewMorale: 'Morale: {m} / 100. A meltdown or a lost Crew fight costs miners and 10 morale.',
  crewForeman: 'Foreman: {name}, {spec}',
  close: 'Close',
  // BP-12b (Wiki copy)
  atBase: 'The sickbay and the Trophy Shelf are kept at base. Open the map to see them.',
  bunks: '{f} fit, {w} wounded, {n} healing, {e} empty bunks',
  fit: 'Fit to fight: {f}',
  bay: 'In the sickbay: {w} wounded.',
  healing: '{n} on their feet in {s} s.',
  back: 'Back on the roster.',
  led: 'Led by {name}. XP {xp}. Stress {n} / 100.',
  kit: 'Kit: standard issue.',
  bank: 'Banked: {n} credits',
  heal: 'Patch up {n} · {cost} credits',
  healNote: '{each} credits a soldier. On their feet {secs} s after payment.',
  short: 'Not enough banked credits. {each} each.',
  requisition: 'Requisition {n} · {cost} credits',
  requisitionNote: '{each} credits a Rookie. They arrive with no history and no complaints yet.',
  hire: 'Hire {n} · {cost} credits',
  hireNote: '{each} credits a miner. Morale stays where it is.',
  out: 'The team is out. Replacements report at base.',
  full: 'Full strength: 8 / 8.',
  sectionFull: 'The Section is already full.',
  crewFull: 'The Crew is already full.',
  shelfKind: 'Loot',
  shelfChip: 'Trophy Shelf',
  shelfTitle: 'Trophy Shelf',
  shelfSub: 'Recovered in the field. Catalogued, polished, not issued. Logistics is still writing the forms.',
  shelfEmpty: 'Nothing recovered yet. Win a fight and something may come home with the team.',
};

// Replace: offered on the player's own rows below 8; one tap buys what the bank covers (BP-12b).
const replaceOffer = (h, each, bank) => h >= 8 ? null : { n: Math.max(1, Math.min(8 - h, Math.floor(bank / each))), afford: bank >= each };

// The heal countdown, against the server's clock (demo_ops.js's serverNow), never the phone's.
const healSecs = (b, now) => Math.max(0, Math.ceil((Date.parse(b.healed_at) - now()) / 1000));

// A5: one line per item the loot roll can produce (loot_roller.rs:20-47, byte for byte). Flavour only: no trophy acts in this build.
const TROPHY_LINES = new Map([
  ["Salvaged Ammo Cache", "Half of it fits our rifles. The other half is evidence."],
  ["Dented Flak Vest", "The dent is where it worked. The previous owner was unavailable for comment."],
  ["Cracked Visor Helmet", "Visibility reduced to a vague sense of optimism."],
  ["Busted Stimpack", "Empty. Someone needed it more, and briefly."],
  ["Scrap Plating", "Logistics lists it as armour. Logistics has never been shot."],
  ["Low-Grade Ore Sample", "Assayed at low grade. Kept for morale, which is also low grade."],
  ["Refurbished Laser Pistol", "Refurbished means it fired once in testing. Twice would be a record."],
  ["Corporate Assault Vest", "Branded on the front, so the enemy knows who to invoice."],
  ["Signal Jammer Module", "Jams signals. Also the kettle, for reasons nobody can explain."],
  ["Pre-Severance Medkit", "Older than the Severance and better stocked than ours."],
  ["Tactical Webbing Rig", "Twelve pouches. Eleven of them hold snacks."],
  ["Gunmetal Iron Ingot (×5)", "Five ingots of the good stuff. Accounting wants them back."],
  ["Military-Grade Laser Carbine", "Military grade. The military would like a word about that."],
  ["Void-Treated Combat Plate", "Hums at night. The Rookies have stopped asking why."],
  ["Cogsmith's Multi-Tool", "Forty functions. The manual covers three of them."],
  ["Psionic Dampener Helm", "Keeps the whispers out. Results vary by whisper."],
  ["Star-Silt Ore Sample", "Glitters in the dark. Do not lick it. There is a form about that."],
  ["Encrypted Corpo Datapad", "Password protected. The password is not 'password'. We tried."],
  ["Pre-Severance Plasma Cutter", "Cuts through hull plate, bedrock and the safety briefing."],
  ['Exo-Frame Shoulder Guard "Bastion"', "One shoulder of a very expensive suit. The rest is still on its owner, somewhere."],
  ["Experimental Breach Charge", "Experimental. The last experiment is now a crater with a name."],
  ["Dark-Core Ore Shard", "Heavier than it looks, darker than it should be. Insured separately."],
  ['Tactical HUD "Ironveil Mk.IV"', "Shows ammo, heading and a performance review. You are underperforming."],
  ["Rogue AI Comms Module", "It says it is not rogue. It says so a lot."],
  ["Sunspear Laser Rifle Mk.V", "Mark Five. Nobody lasted long enough to file feedback on the first four."],
  ['Void-Weave Exosuit "Relic"', "Woven from something that should not take thread. It fits everyone, which is the worrying part."],
  ["Emperor's Ore Pendant (smells faintly of bin chicken)", "Possibly holy. Certainly fragrant. Kept in a sealed bag, for everyone's sake."],
  ["Pre-Severance Jump Drive Fragment", "A piece of a ship that went somewhere fast. The rest of it is still going."],
  ["Squeaker's Signed Biscuit (DO NOT EAT)", "Signed in what we hope is icing. Under armed guard. Do not eat."],
]);
const TROPHY_UNKNOWN = "Catalogued. Nobody is sure what it does.";

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
.company-card-lines { display: flex; flex-direction: column; gap: 2cqw; max-height: 100cqw; overflow-y: auto; }
.company-card-lines[hidden], .company-card-line[hidden], .company-act[hidden], .company-trophy[hidden] { display: none; }
.company-bunks { font-size: 5cqw; letter-spacing: 0.6cqw; }
.company-card .company-act { width: 100%; max-width: none; margin-top: 0; min-height: 11cqw; font-size: 4.4cqw; }
.company-trophy { display: flex; flex-direction: column; gap: 0.6cqw; padding: 1.6cqw 0; border-top: 0.3cqw solid var(--panel-edge); }
.company-trophy-name { margin: 0; font-family: var(--display); font-weight: 800; font-size: 4.2cqw; line-height: 1.2; overflow-wrap: anywhere; }
.company-trophy-grade { margin: 0; font-family: var(--mono); font-size: 3.2cqw; text-transform: uppercase; color: var(--accent); }
.company-trophy-line { margin: 0; font-size: 3.6cqw; line-height: 1.35; color: var(--muted); }
`;

const fill = (text, values) => text.replace(/\{(\w+)\}/g, (m, k) => (k in values ? String(values[k]) : m));

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

let deps = null;
let view = null;      // demo_ops.js's state and requests (BP-12b), registered at the mount
let fromHub = false;  // the reveal about to render was opened by the hub's Your Company
let card = null;      // the stats card: { node, title, lines }
let traitsLine = null; // the briefing's trait lines, created on the first briefing
let shown = null;     // the card on screen: { kind, index }, or null
let tick = null;      // the heal countdown's display timer (no network call)
let drawnKey = null;  // the open card's drawn state, the countdown left out (Reviewer BP-12b F1)

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
  const opened = fromHub;
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
  // A5: the Trophy Shelf's chip, from the hub only, once an ops body has been adopted (BP-12b).
  const list = container.querySelector('ul.chips');
  if (opened && list && view && view.adopted()) {
    const shelf = el('li', 'chip');
    shelf.append(el('span', 'chip-kind', COMPANY_COPY.shelfKind), el('span', 'chip-main', COMPANY_COPY.shelfChip));
    shelf.dataset.company = 'shelf';
    shelf.dataset.index = '0';
    shelf.setAttribute('role', 'button');
    shelf.setAttribute('tabindex', '0');
    list.append(shelf);
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

function stopTick() {
  if (tick !== null) clearTimeout(tick);
  tick = null;
}

function hideCard() {
  stopTick();
  shown = null;
  drawnKey = null;
  if (card) card.node.hidden = true;
}

const line = (text, muted) => el('p', muted ? 'company-card-line muted' : 'company-card-line', text);

// A row's drawn state: its text (the countdown row's seconds left out) and each button's enabled flag.
const drawn = n => (n.dataset.tick ? 'tick' : n.textContent) + '|' + [n, ...n.querySelectorAll('button')].filter(b => b.tagName === 'BUTTON').map(b => (b.disabled ? 0 : 1)).join('');

// A heal or replace button: `onTap` runs only while it is enabled.
function act(text, disabled, onTap) {
  const b = el('button', 'btn company-act', text);
  b.type = 'button';
  b.disabled = Boolean(disabled);
  b.addEventListener('click', () => { if (!b.disabled) onTap(); });
  return b;
}

// The last refusal's line for this card and action, if demo_ops.js holds one (BP-12b).
function noticeLine(target, action, each) {
  const n = view.notice();
  if (!n || n.target !== target || n.action !== action) return null;
  if (n.kind === 'short') return fill(COMPANY_COPY.short, { each });
  if (n.kind === 'out') return COMPANY_COPY.out;
  if (n.kind === 'full') return target === 'section' ? COMPANY_COPY.sectionFull : COMPANY_COPY.crewFull;
  return null;
}

// Replace (B2's offer): full, or one button with its note, the short line and the out line.
function replaceRows(target, id, h, each, label, note) {
  const offer = replaceOffer(h, each, view.credits());
  if (offer === null) {
    const full = noticeLine(target, 'replace', each); // a "no room" refusal, after its roster re-read
    return full ? [line(COMPANY_COPY.full), line(full)] : [line(COMPANY_COPY.full)];
  }
  const out = target === 'section' ? view.out() && !view.sectionHome() : view.out(); // BP-17b: a Section at base on an unguarded dig takes Rookies; a Crew out never hires
  const isHeld = view.held();
  const btn = act(fill(label, { n: offer.n, cost: offer.n * each }), false, () => view.replace(target, id, offer.n, each));
  btn.disabled = !offer.afford || out || isHeld;
  const rows = [btn, line(fill(note, { each }), true)];
  const texts = [];
  if (!offer.afford) texts.push(fill(COMPANY_COPY.short, { each }));
  if (out) texts.push(COMPANY_COPY.out);
  const extra = noticeLine(target, 'replace', each);
  if (extra && !texts.includes(extra)) texts.push(extra);
  return rows.concat(texts.map(t => line(t)));
}

// The Section card once an ops body is adopted: bunks, sickbay, the leader, kit, the bank, Heal, Replace.
function sectionRows(s) {
  const h = Math.min(8, Math.max(0, Number(s.headcount) || 0));
  const raw = view.bay(s.section_id);   // the body's own entry
  const b = view.bayOf(s.section_id);   // a batch past healed_at counts as back
  const fit = Math.max(0, h - b.wounded - b.healing);
  const credits = view.credits();
  const rows = [line(fill(COMPANY_COPY.sectionRookies, { n: s.headcount }))];
  const bunks = line('🪖'.repeat(fit) + '⏳'.repeat(b.healing) + '🩹'.repeat(b.wounded) + '·'.repeat(Math.max(0, 8 - h)));
  bunks.classList.add('company-bunks');
  bunks.setAttribute('aria-label', fill(COMPANY_COPY.bunks, { f: fit, w: b.wounded, n: b.healing, e: Math.max(0, 8 - h) }));
  rows.push(bunks);
  if (fit !== h) rows.push(line(fill(COMPANY_COPY.fit, { f: fit })));
  if (b.wounded > 0) rows.push(line(fill(COMPANY_COPY.bay, { w: b.wounded })));
  let ticking = false;
  const batch = Number(raw.healing) || 0;
  if (batch > 0 && raw.healed_at) {
    const secs = healSecs(raw, view.serverNow);
    if (secs > 0) {
      const t = line(fill(COMPANY_COPY.healing, { n: batch, s: secs }));
      t.dataset.tick = '1';
      rows.push(t);
      ticking = true;
    } else {
      rows.push(line(COMPANY_COPY.back));
    }
  }
  const c = campaignCommander();
  if (c) rows.push(line(fill(COMPANY_COPY.led, { name: c.name, xp: xpLine(c.xp, c.rank), n: c.stress }), true));
  rows.push(line(COMPANY_COPY.kit, true), line(fill(COMPANY_COPY.bank, { n: credits })));
  if (b.wounded > 0 && b.healing === 0) {
    const p = healPrice(view.base());
    const cost = b.wounded * p.each;
    rows.push(act(fill(COMPANY_COPY.heal, { n: b.wounded, cost }), credits < cost || view.held(), () => view.heal(s.section_id)));
    rows.push(line(fill(COMPANY_COPY.healNote, { each: p.each, secs: p.secs }), true));
    const texts = credits < cost ? [fill(COMPANY_COPY.short, { each: p.each })] : [];
    const extra = noticeLine('section', 'heal', p.each);
    if (extra && !texts.includes(extra)) texts.push(extra);
    rows.push(...texts.map(t => line(t)));
  }
  rows.push(...replaceRows('section', s.section_id, h, REPLACE_MERC_CREDITS, COMPANY_COPY.requisition, COMPANY_COPY.requisitionNote));
  return { rows, ticking };
}

function crewRows(w, r) {
  const rows = [line(fill(COMPANY_COPY.crewMiners, { n: w.headcount })), line(fill(COMPANY_COPY.crewMorale, { m: w.morale }))];
  const f = Array.isArray(r.foremen) ? r.foremen[0] : null;
  if (f) rows.push(line(fill(COMPANY_COPY.crewForeman, { name: f.name, spec: f.specialization }), true));
  if (!view || !view.adopted()) return rows;
  rows.push(line(fill(COMPANY_COPY.bank, { n: view.credits() })));
  const h = Math.min(8, Math.max(0, Number(w.headcount) || 0));
  return rows.concat(replaceRows('crew', w.crew_id, h, REPLACE_MINER_CREDITS, COMPANY_COPY.hire, COMPANY_COPY.hireNote));
}

// A5: newest first (the body lists newest last), each with its grade and its line.
function shelfRows() {
  const items = Array.isArray(view.trophies()) ? view.trophies().slice().reverse() : [];
  const rows = [line(COMPANY_COPY.shelfSub, true)];
  if (items.length === 0) return rows.concat(line(COMPANY_COPY.shelfEmpty));
  for (const t of items) {
    const row = el('div', 'company-trophy');
    row.append(el('p', 'company-trophy-name', String(t.item_name)), el('p', 'company-trophy-grade', String(t.quality_grade)),
      el('p', 'company-trophy-line', TROPHY_LINES.get(t.item_name) || TROPHY_UNKNOWN));
    rows.push(row);
  }
  return rows;
}

function openCard(kind, index) {
  const r = roster();
  if (!r) return;
  if (!shown || shown.kind !== kind || shown.index !== index) view.clearNotice();
  stopTick();
  let rows = [];
  let title = '';
  let ticking = false;
  let id = '';
  if (kind === 'section') {
    const s = Array.isArray(r.sections) ? r.sections[index] : null;
    if (!s) return;
    title = s.name;
    id = String(s.section_id);
    if (view && view.adopted()) {
      ({ rows, ticking } = sectionRows(s));
    } else {
      rows = [line(fill(COMPANY_COPY.sectionRookies, { n: s.headcount })), line(COMPANY_COPY.sectionHow, true), line(COMPANY_COPY.sectionLoss, true), line(COMPANY_COPY.atBase, true)];
    }
  } else if (kind === 'crew') {
    const w = Array.isArray(r.crews) ? r.crews[index] : null;
    if (!w) return;
    title = COMPANY_COPY.crewTitle;
    id = String(w.crew_id);
    rows = crewRows(w, r);
  } else if (kind === 'shelf') {
    if (!view || !view.adopted()) return;
    title = COMPANY_COPY.shelfTitle;
    rows = shelfRows();
  } else {
    return;
  }
  // F1: redraw only when what the card draws changes; a tick alone rewrites the countdown in place.
  const key = [kind, index, id, title].concat(rows.map(drawn)).join('\n');
  const same = shown && shown.kind === kind && shown.index === index && key === drawnKey && !card.node.hidden;
  if (same) {
    const fresh = rows.find(n => n.dataset.tick);
    const live = card.lines.querySelector('[data-tick]');
    if (fresh && live) live.textContent = fresh.textContent;
  } else {
    card.title.textContent = title;
    card.lines.replaceChildren(...rows);
    drawnKey = key;
  }
  card.node.hidden = false;
  shown = { kind, index };
  if (ticking) tick = setTimeout(() => { tick = null; if (shown) openCard(shown.kind, shown.index); }, 1000);
}

// demo_ops.js calls this after every adopted body and every refresh, so the buttons follow held()
// (BP-12b spec check H3): the open card is drawn again from the state it now holds.
export function renderCompany() {
  if (card && shown && !card.node.hidden) openCard(shown.kind, shown.index);
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
  const n = s ? view.squadOf(s) : 0; // the squad sent (BP-12b: the headcount less the sickbay)
  const labels = [...lines.querySelectorAll('li.score-row .score-label')];
  // The roll's own rows say whether the Gunsmith stood (roll_inputs.gunsmith, read once by the server): no base read.
  const gun = labels.some(l => l.textContent === GUNSMITH_ROW);
  for (const label of labels) {
    const key = label.textContent;
    if (key === 'Gear' && !gun) label.textContent = GEAR_HINT;
    else if (Object.prototype.hasOwnProperty.call(SCORE_LABELS, key)) label.textContent = fill(SCORE_LABELS[key], { n });
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

// `d`: demo_ops.js's deps (main.js's $, getRoster, ...); `v`: demo_ops.js's view of its state
// (adopted, credits, out, sectionHome, held, serverNow, bay, bayOf, squadOf, trophies, notice, clearNotice,
// heal, replace).
export function mountCompany(d, v) {
  deps = d;
  view = v;
  const style = document.createElement('style');
  style.textContent = CSS;
  document.head.append(style);
  deps.$('btn-back').textContent = COMPANY_COPY.hubButton;
  // Taken before the keeper's own listener renders the reveal (its observer runs straight after that listener).
  deps.$('screen-hub').addEventListener('click', e => {
    if (e.target instanceof Element && e.target.closest('#btn-back')) fromHub = true;
  }, true);
  const reveal = deps.$('reveal-content');
  const screen = deps.$('screen-reveal');
  card = createCard();
  screen.append(card.node);
  // Reviewer BP-11b-2 F1: whenever the reveal is hidden (main.js's show() sets every screen's
  // hidden on every call), a Back that rendered no reveal leaves no stale fromHub, and the card
  // (with its tick) closes. Created before the content observer, so a Back's own render, which
  // shows the reveal before this runs, keeps the flag.
  new MutationObserver(() => { if (screen.hidden) { fromHub = false; hideCard(); } }).observe(screen, { attributes: true, attributeFilter: ['hidden'] });
  new MutationObserver(() => dressReveal(reveal)).observe(reveal, { childList: true, subtree: true });
  reveal.addEventListener('click', chipAction);
  reveal.addEventListener('keydown', chipAction);
  const aar = deps.$('aar-commander');
  new MutationObserver(() => dressAar(aar)).observe(aar, { childList: true });
  const lines = deps.$('battle-lines');
  new MutationObserver(() => dressScore(lines)).observe(lines, { childList: true });
}
