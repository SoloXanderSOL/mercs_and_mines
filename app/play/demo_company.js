// demo-only (BP-11b-2): the numbers given their meaning (Breakpoint §4e Tier A: A6, A7, A8, A10, A11).
// Never ported to dev: the port deletes this file.
//
// The keeper screens draw their own copy; this file rewrites it where the demo needs more, by
// watching the keeper's DOM (no keeper code calls in here). XP is shown against the next rank's
// threshold (pinned below, and by the harness from commander_gen.rs), stress against 100, a rank-up
// says what it really changes in this build, each trait gets a plain line, the hub's Back opens
// "Your Company" with a stats card for the Section and the Crew, and the combat screen's factor
// labels say where each number comes from. It reads the roster the client already holds and makes
// no network call but BP-24b's name ask. Every string goes in through textContent.
//
// BP-12b: "Your Company" is the Company screen. Once demo_ops.js has adopted an ops body for the
// campaign, the Section card shows its bunks, its sickbay, Heal and Replace, the Crew card shows
// Replace, and a Trophy Shelf chip lists the recovered loot. The state and every request are
// demo_ops.js's, read through the view it registers; this file renders, keeps the costs and the
// shelf's lines, and ticks the heal countdown (no network call) while the card is open.

import { store, request, ApiError, setOnJoined } from './api.js';
import { bandWord, bankedLine } from './demo_dig.js'; // BP-19c; BP-28b: the bank line, the hub chip's own

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
export const SELL_ORE_CREDITS = 2;      // demo_ops.rs DEMO_ORE_SELL_CREDITS (BP-28a); harness sl cases hold their own copy

// A7: what a rank really changes in this build (since B5, each rank above 1 is also +2 on every roll).
const RANK_LINES = {
  2: 'Rank 2. +2 on every roll. Cleared to command 4 Sections. The paperwork is very impressive.',
  3: 'Rank 3. +4 on every roll. A death on the relic dig or the Black Site now goes down as glorious.',
  4: 'Rank 4. +6 on every roll. Cleared for 64 Sections. Command admires the optimism.',
  5: 'Rank 5. Top rank, +8 on every roll. Any death now goes down as glorious.',
};

// BP-19c: what a Foreman's rank changes in this build: +2 on the dig's heat limit a rank above 1, none when Shattered
// (demo_ops.rs DEMO_FOREMAN_RANK_THRESHOLD, foreman_threshold). Shown on the Crew card by current rank.
const FOREMAN_RANK_LINES = {
  2: 'Rank 2. The heat limit is 2 higher. The rig runs hotter before it melts.',
  3: 'Rank 3. The heat limit is 4 higher.',
  4: 'Rank 4. The heat limit is 6 higher.',
  5: 'Rank 5. Top rank, the heat limit is 8 higher.',
};
// BP-19c (Reviewer BP-19a: the copy that explains a tired Foreman's shorter heat bar): what a band does to the dig's
// heat limit, keyed by the roster's band (demo_ops.rs foreman_threshold: DEMO_STRAINED_THRESHOLD_CUT 5,
// DEMO_BREAKING_THRESHOLD_CUT 10, the Coolant-Jockey's 15 halved while Strained). A retune updates the server
// constant and this copy in one commit (Reviewer BP-16b).
const FOREMAN_BAND_LINES = {
  strained: 'Strained: the heat limit is 5 lower, and a Coolant-Jockey gives half the usual bonus.',
  breaking_point: 'Breaking Point: the heat limit is 10 lower, and no Coolant-Jockey bonus.',
  shattered: 'Shattered: the heat limit is 10 lower, with no bonus from rank or Coolant-Jockey.',
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
// their own labels and pass through untouched. Gear stays its own line; the armoury's kit is the server's own row (BP-23b).
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
const KIT_ROW = 'Armoury kit ('; // BP-23b: demo_roll.rs:351's label, shown as sent; with it the hint steps aside too
// BP-24b: ops/name's four 400 texts (demo_name.rs:51-54), shown as sent; any other refusal shows askBad.
const NAME_REFUSALS = new Set(['a company name is 1 to 24 characters', "letters, numbers, spaces and - ' . & only",
  'a company name needs at least one letter', 'Compliance has rejected that name. Pick another.']);

const COMPANY_COPY = {
  hubButton: 'Your Company',
  heading: 'Your Company',
  // BP-24b (Wiki copy): the name ask on the reveal after a new run's join (a 201), and its answers.
  askLine: 'Name your company. Compliance will read it.',
  askPlaceholder: 'Your Company',
  askName: 'Name it',
  askSkip: 'Not now',
  askDone: 'Registered with Compliance:',
  askBad: 'That name did not go through. Try another.',
  askClosed: 'Compliance is closed. The company stays Your Company this run.',
  xpNext: '{xp} / {next} to Rank {r}',
  xpTop: '{xp} · top rank',
  stress: '{n} / 100',
  stressNote: 'At 100 they crack. The run ends there.',
  // BP-19c (Wiki copy): the band words, the bench, and the Foreman on the Crew card.
  stressBand: '{n} / 100 · {band}',
  restNote: 'Rest at base sheds stress, a little every minute. The figure catches up at the next report.',
  ledBand: 'Led by {name}. XP {xp}. Stress {n} / 100, {band}.',
  foremanRank: 'Rank {r} · XP {xp}',
  foremanStress: 'Stress {n} / 100',
  foremanStressBand: 'Stress {n} / 100 · {band}',
  foremanRecovers: 'In this demo only, rest at base brings a Shattered Foreman back to Strained at 70 stress. Command calls it a sabbatical.',
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
  crewMorale: 'Morale: {m} / 100. Meltdowns, lost Crew fights and bad roads all cost morale.', // BP-20b (Wiki copy)
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
  kitHeld: 'Kit: {item} ({grade}), +{p} on the roll.', // BP-23b: p is company.kit's points, the server's own
  kitOff: 'Take off',
  // BP-28b (Wiki copy): selling banked ore. The bank line is demo_dig.js's bankedLine, as on the hub chip.
  sell: 'Sell ore',
  sellAsk: 'Sell {o} ore for {c} credits?',
  sellLess: '-10',
  sellMore: '+10',
  sellGo: 'Sell',
  sellCancel: 'Cancel',
  sold: 'Sold {o} ore for {c} credits. Command thanks you for your liquidity.',
  oreShort: 'There is less ore in the bank than that. The figures are up to date now.',
  heal: 'Patch up {n} · {cost} credits',
  healNote: '{each} credits a soldier. On their feet {secs} s after payment.',
  short: 'Not enough banked credits. {each} each.',
  requisition: 'Requisition {n} · {cost} credits',
  requisitionNote: '{each} credits a Rookie. They arrive with no history and no complaints yet.',
  hire: 'Hire {n} miners · {cost} credits',
  hireNote: '{each} credits a miner. Morale stays where it is.',
  out: 'A squad is out. Miners sign on when every squad is home.',
  // BP-18b-1 (Wiki copy)
  sectionOut: 'This Section is out. Replacements report at base.',
  squadHire: 'Hire a squad: a Commander and 8 mercs, {credits} credits.',
  squadHireBtn: 'Hire squad',
  hireCap: 'Two squads is the most you can field.',
  hireShort: 'Not enough banked credits. A squad costs {credits}.',
  hired: 'A Commander and 8 mercs report for duty.',
  wages: 'Wages: {n} credits a minute.',
  wagesNote: 'Demo wages, worked out when you check in, never below 0. A Section with nobody left still draws pay.',
  full: 'Full strength: 8 / 8.',
  sectionFull: 'The Section is already full.',
  crewFull: 'The Crew is already full.',
  shelfKind: 'Loot',
  shelfChip: 'Trophy Shelf',
  shelfTitle: 'Trophy Shelf',
  shelfSub: 'Recovered in the field. One item per Section; it counts in every fight that Section leads.',
  shelfEmpty: 'Nothing recovered yet. Win a fight and something may come home with the team.',
  // BP-23b (Wiki copy): the armoury
  equip: 'Equip',
  equipTo: 'Equip {name}',
  kitAtBase: 'Kit is issued at base. A Section that is out keeps what it carries.',
  kitGone: 'That item has left the shelf. The list is up to date now.',
  kitShelfFull: 'The Trophy Shelf is full. Swap in another item instead.',
};

// Replace: offered on the player's own rows below 8; one tap buys what the bank covers (BP-12b).
const replaceOffer = (h, each, bank) => h >= 8 ? null : { n: Math.max(1, Math.min(8 - h, Math.floor(bank / each))), afford: bank >= each };

// The heal countdown, against the server's clock (demo_ops.js's serverNow), never the phone's.
const healSecs = (b, now) => Math.max(0, Math.ceil((Date.parse(b.healed_at) - now()) / 1000));

// A5: one line per item the loot roll can produce (loot_roller.rs:20-47, byte for byte). The line is flavour: an equipped item's points are the server's (company.kit, BP-23b).
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
.portrait .company-portrait { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; }
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
.company-equip { display: flex; flex-wrap: wrap; gap: 1.5cqw; }
.company-card .company-equip .company-act { flex: 1 1 0; width: auto; min-height: 8cqw; padding: 0 2cqw; font-size: 3.6cqw; }
.company-hire { display: flex; flex-direction: column; gap: 1.5cqw; margin-top: 3cqw; text-align: left; }
.company-hire .company-card-line { font-size: 3.8cqw; }
.company-hire .company-card-line.muted { font-size: 3.4cqw; }
.company-hire .company-act { width: 100%; max-width: none; margin-top: 0; min-height: 11cqw; font-size: 4.4cqw; }
.reveal-heading.company-heading { overflow-wrap: anywhere; }
.name-ask { display: flex; flex-direction: column; gap: 2cqw; text-align: left; }
.name-ask-line { margin: 0; font-size: 4.2cqw; line-height: 1.4; color: var(--accent); }
.name-ask-input { width: 100%; min-height: 13cqw; padding: 2.5cqw 3cqw; font-family: var(--body); font-size: 5cqw; text-transform: none; color: var(--text); background: var(--panel); border: 0.5cqw solid var(--panel-edge); border-radius: 1.5cqw; }
.name-ask-input:focus-visible { outline: 0.6cqw solid var(--text); outline-offset: 0.4cqw; }
/* BP-29a: a yellow caret, and a yellow pulse while the box is empty (focused or not: the ask focuses it), so the box is found */
/* whether or not the phone opens its keyboard. box-shadow, so the :focus-visible outline above is left alone; steady when reduced. */
.name-ask-input { caret-color: var(--accent); }
.name-ask-input:placeholder-shown { animation: name-ask-pulse 1.6s ease-in-out infinite; }
@keyframes name-ask-pulse { 0%, 100% { box-shadow: 0 0 0 0 transparent; } 50% { box-shadow: 0 0 0 0.8cqw var(--accent); } }
@media (prefers-reduced-motion: reduce) { .name-ask-input:placeholder-shown { animation: none; box-shadow: 0 0 0 0.5cqw var(--accent); } }
.name-ask-note { margin: 0; font-size: 3.8cqw; line-height: 1.35; color: var(--accent); }
.name-ask-note[hidden] { display: none; }
.name-ask-row { display: flex; gap: 2cqw; }
.name-ask-row .btn { flex: 1 1 0; width: auto; min-height: 11cqw; padding: 0 2cqw; font-size: 4.4cqw; }
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
let companyUp = false; // BP-20b (Reviewer BP-19c F3): the reveal on screen is Your Company, not an entry reveal
let card = null;      // the stats card: { node, title, lines }
let traitsLine = null; // the briefing's trait lines, created on the first briefing
let shown = null;     // the card on screen: { kind, index }, or null
let tick = null;      // the heal countdown's display timer (no network call)
let drawnKey = null;  // the open card's drawn state, the countdown left out (Reviewer BP-12b F1)
let selling = null;   // BP-28b: the open Sell row: { key, ore }, the amount Sell will post
let sold = null;      // BP-28b: the last sale's line, on the card that made it: { key, ore }
let hireBox = null;   // BP-18b-1: the squad hire block on the Company screen

// BP-24b: the name ask. A 201 join marks its campaign, in sessionStorage so a reload mid-ask asks again, with an
// in-page Set for private mode; Name it's answers, Not now and Continue settle it. Keyed on the join, never on
// company.name being absent: the name is off the keep list and can vanish mid-run (Reviewer BP-24a F7).
const askKey = id => `mm.demo.nameAsk.${id}`;
const asks = new Set();
function askPending(id) {
  if (asks.has(id)) return true;
  try { return sessionStorage.getItem(askKey(id)) === '1'; } catch { return false; }
}
function markAsk(id) {
  asks.add(id);
  try { sessionStorage.setItem(askKey(id), '1'); } catch { /* the in-page Set holds it */ }
}
function settleAsk(id) {
  asks.delete(id);
  try { sessionStorage.removeItem(askKey(id)); } catch { /* memory only */ }
}

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

// BP-18b-1: the wage line, from the body's company.wages (the one reader; Reviewer BP-18a-2 F3: never its
// cached squad count). '' when the body carries none.
export function wageLine(company) {
  const w = company && company.wages;
  const n = w ? Number(w.per_minute) : NaN;
  return Number.isFinite(n) ? fill(COMPANY_COPY.wages, { n }) : '';
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

// BP-22 (art list #28-32): one painted portrait per origin, laid over the keeper card's silhouette. An origin
// not in the map, or a file that fails to load, leaves the silhouette. Both reveals (the join's and Your
// Company) dress the same card. The swap re-fires the observer; dressReveal's company flag absorbs it.
const PORTRAITS = {
  'Human (Corporate)': 'portrait_human_corporate.jpg',
  'Human (Underhive)': 'portrait_human_underhive.jpg',
  Duck: 'portrait_duck.jpg',
  Bearocrat: 'portrait_bearocrat.jpg',
  'Science Ape': 'portrait_science_ape.jpg',
};

function dressPortrait(cardNode) {
  const box = cardNode.querySelector('.portrait');
  const row = statRow(cardNode, 'Origin');
  const origin = row && row.querySelector('.stat-value').textContent;
  if (!box || !origin || !Object.prototype.hasOwnProperty.call(PORTRAITS, origin)) return;
  const img = el('img', 'company-portrait');
  img.alt = '';
  img.addEventListener('error', () => img.remove());
  img.src = `assets/art/${PORTRAITS[origin]}`;
  box.append(img);
}

function dressReveal(container) {
  const cardNode = container.querySelector('.commander-card');
  if (!cardNode || cardNode.dataset.company === '1') return;
  cardNode.dataset.company = '1';
  dressPortrait(cardNode);
  hideCard();
  const opened = fromHub;
  companyUp = opened; // BP-20b (F3)
  if (fromHub) {
    fromHub = false;
    const heading = container.querySelector('.reveal-heading');
    if (heading) {
      // BP-24b: the adopted body's company.name, else Your Company (a name lost to an eviction or a deploy falls
      // back here, with no ask). Text only (Reviewer BP-24a F6); dir auto for a right-to-left name (F5).
      const company = view && view.adopted() ? view.company() : null;
      heading.textContent = company && typeof company.name === 'string' ? company.name : COMPANY_COPY.heading;
      heading.dir = 'auto';
      heading.classList.add('company-heading');
    }
  }
  const askCamp = store.campaign(); // BP-24b: an entry reveal (never the Company screen) carries a pending ask
  if (!opened && askCamp && askPending(askCamp.campaign_id)) drawAsk(container, askCamp.campaign_id);
  const c = campaignCommander();
  const xpRow = statRow(cardNode, 'XP');
  if (c && xpRow) xpRow.querySelector('.stat-value').textContent = xpLine(c.xp, c.rank);
  const stressRow = statRow(cardNode, 'Stress');
  if (c && stressRow) {
    const band = bandWord(c.band); // BP-19c
    stressRow.querySelector('.stat-value').textContent = fill(band ? COMPANY_COPY.stressBand : COMPANY_COPY.stress, { n: c.stress, band });
    const note = el('p', 'company-note', COMPANY_COPY.stressNote);
    cardNode.after(note);
    if (band) note.after(el('p', 'company-note', COMPANY_COPY.restNote)); // the bench came with the band (0.3.3)
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
  // BP-18b-1: the squad hire, once, above the chips (from the hub, with an adopted body that carries company.hire):
  // #reveal-content scrolls with it below them (measured: scrollHeight 949 to 1024 against clientHeight 600).
  hireBox = null;
  if (opened && list && view && view.adopted() && view.company().hire) {
    hireBox = el('div', 'company-hire');
    list.before(hireBox);
    drawHire();
  }
}

// BP-24b: the name ask, under the reveal's heading. Name it sends the raw value (the server trims and checks it)
// to the stored campaign and adopts nothing: the hub's next ops read brings company.name. A 400/422/415 keeps the
// ask (the server's four texts shown as sent, else askBad); every other answer settles it with askClosed.
function drawAsk(container, id) {
  const heading = container.querySelector('.reveal-heading');
  if (!heading) return;
  const box = el('div', 'name-ask');
  const input = el('input', 'name-ask-input');
  input.type = 'text';
  input.maxLength = 24;
  input.autocomplete = 'off';
  input.setAttribute('autocapitalize', 'words');
  input.spellcheck = false;
  input.setAttribute('enterkeyhint', 'done');
  input.placeholder = COMPANY_COPY.askPlaceholder;
  input.setAttribute('aria-label', COMPANY_COPY.askLine);
  const note = el('p', 'name-ask-note');
  note.hidden = true;
  note.setAttribute('role', 'status');
  const nameBtn = el('button', 'btn btn-primary name-ask-name', COMPANY_COPY.askName);
  const skipBtn = el('button', 'btn name-ask-skip', COMPANY_COPY.askSkip);
  nameBtn.type = 'button';
  skipBtn.type = 'button';
  const row = el('div', 'name-ask-row');
  row.append(nameBtn, skipBtn);
  box.append(el('p', 'name-ask-line', COMPANY_COPY.askLine), input, note, row);
  heading.after(box);
  let busy = false;
  const ready = () => { nameBtn.disabled = busy || input.value.trim() === ''; skipBtn.disabled = busy; input.disabled = busy; };
  const close = text => {
    settleAsk(id);
    if (!text) return box.remove();
    box.replaceChildren(el('p', 'name-ask-line', text));
  };
  ready();
  input.focus({ preventScroll: true }); // BP-29a (feedback item 8): the caret shows where to type; the phone may open its keyboard
  input.addEventListener('input', () => { note.hidden = true; ready(); });
  input.addEventListener('keydown', e => { if (e.key === 'Enter' && !nameBtn.disabled) { e.preventDefault(); nameBtn.click(); } });
  skipBtn.addEventListener('click', () => { if (!skipBtn.disabled) close(null); });
  nameBtn.addEventListener('click', async () => {
    if (nameBtn.disabled) return;
    busy = true;
    ready();
    try {
      const r = await request('POST', `/api/campaign/${encodeURIComponent(id)}/ops/name`, { body: { name: input.value }, retry500: false });
      const named = r.body && r.body.company && typeof r.body.company.name === 'string' ? r.body.company.name : input.value.trim();
      close(`${COMPANY_COPY.askDone} ${named}`);
    } catch (e) {
      if (e instanceof ApiError && e.kind === 'bad') {
        busy = false;
        note.textContent = NAME_REFUSALS.has(e.message) ? e.message : COMPANY_COPY.askBad;
        note.hidden = false;
        ready();
        input.focus({ preventScroll: true }); // BP-29a (Reviewer BP-24b F5): back in the box after a refusal
        return;
      }
      close(COMPANY_COPY.askClosed);
    }
  });
}

// The hire block: its line, Hire squad, the cap or short line, the hire's refusal, the hired line, the wage
// line and the demo note. The squad count is the roster's Sections, never the body's (Reviewer BP-18a-2 F3).
function drawHire() {
  if (!hireBox || !hireBox.isConnected || !view) return;
  const hire = view.company().hire;
  if (!hire) return hireBox.replaceChildren();
  const r = roster();
  const squads = r && Array.isArray(r.sections) ? r.sections.length : 0;
  const capped = squads >= Number(hire.cap);
  const short = view.credits() < Number(hire.credits);
  const rows = [line(fill(COMPANY_COPY.squadHire, { credits: hire.credits })),
    act(COMPANY_COPY.squadHireBtn, capped || short || view.held(), () => view.hire())];
  const texts = [];
  if (capped) texts.push(COMPANY_COPY.hireCap);
  else if (short) texts.push(fill(COMPANY_COPY.hireShort, { credits: hire.credits }));
  const n = view.notice();
  const refused = n && n.action === 'hire' ? (n.kind === 'cap' ? COMPANY_COPY.hireCap : n.kind === 'short' ? fill(COMPANY_COPY.hireShort, { credits: hire.credits }) : null) : null;
  if (refused && !texts.includes(refused)) texts.push(refused);
  if (view.hired()) texts.push(COMPANY_COPY.hired);
  const wages = wageLine(view.company());
  if (wages) texts.push(wages);
  rows.push(...texts.map(t => line(t)), line(COMPANY_COPY.wagesNote, true));
  hireBox.replaceChildren(...rows);
}

// After a squad hire's roster re-read (demo_ops.js): the open Company screen is drawn again from the new
// roster through the hub's own Your Company path, so the new Section's chip and the hire block both redraw.
export function redrawCompany() {
  if (!deps || !companyUp || deps.$('screen-reveal').hidden) return; // BP-20b (F3): never Back off an entry reveal
  deps.$('btn-back').click();
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
  selling = null; // BP-28b: a closed card forgets its Sell row and its sold line
  sold = null;
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
  if (action === 'equip') return n.kind === 'out' ? COMPANY_COPY.kitAtBase : n.kind === 'gone' ? COMPANY_COPY.kitGone : n.kind === 'shelfFull' ? COMPANY_COPY.kitShelfFull : null; // BP-23b
  if (n.kind === 'short') return fill(COMPANY_COPY.short, { each });
  if (n.kind === 'out') return target === 'section' ? COMPANY_COPY.sectionOut : COMPANY_COPY.out;
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
  const out = target === 'section' ? view.sectionOut(id) : view.out(); // BP-18b-1: a Section is out only while an op holds it; a Crew never hires with any op out
  const isHeld = view.held();
  const btn = act(fill(label, { n: offer.n, cost: offer.n * each }), false, () => view.replace(target, id, offer.n, each));
  btn.disabled = !offer.afford || out || isHeld;
  const rows = [btn, line(fill(note, { each }), true)];
  const texts = [];
  if (!offer.afford) texts.push(fill(COMPANY_COPY.short, { each }));
  if (out) texts.push(target === 'section' ? COMPANY_COPY.sectionOut : COMPANY_COPY.out);
  const extra = noticeLine(target, 'replace', each);
  if (extra && !texts.includes(extra)) texts.push(extra);
  return rows.concat(texts.map(t => line(t)));
}

// BP-28b: the bank line and Sell ore, on the Section and the Crew card alike (`key` names the card's row). Closed: one
// Sell ore button while ore is banked. Open: the ask with its price, -10 / +10 (1 to all banked, starting at all; no
// text box, so no phone keyboard) and Sell / Cancel. Sell closes the row: a 200 draws the sold line, a 409 the oreShort
// line after send's re-read. A sale is allowed with a team out (BP-28a: nothing holds banked ore), so no out-guard.
function bankRows(key) {
  const ore = Math.max(0, Number(view.ore()) || 0);
  const rows = [line(bankedLine(ore, view.credits()))];
  const redraw = () => { if (shown) openCard(shown.kind, shown.index); };
  if (selling && (selling.key !== key || ore === 0)) selling = null;
  if (selling) selling.ore = Math.min(Math.max(1, selling.ore), ore);
  const isHeld = view.held();
  if (!selling) {
    if (ore > 0) rows.push(act(COMPANY_COPY.sell, isHeld, () => { selling = { key, ore }; sold = null; view.clearNotice(); redraw(); }));
  } else {
    const o = selling.ore;
    const step = d => () => { if (selling) selling.ore = Math.min(ore, Math.max(1, selling.ore + d)); redraw(); };
    rows.push(line(fill(COMPANY_COPY.sellAsk, { o, c: o * SELL_ORE_CREDITS })));
    const steps = el('div', 'company-equip');
    steps.append(act(COMPANY_COPY.sellLess, isHeld || o <= 1, step(-10)), act(COMPANY_COPY.sellMore, isHeld || o >= ore, step(10)));
    const go = el('div', 'company-equip');
    go.append(act(COMPANY_COPY.sellGo, isHeld, () => { selling = null; view.sell(o, () => { sold = { key, ore: o }; renderCompany(); }); }),
      act(COMPANY_COPY.sellCancel, false, () => { selling = null; redraw(); }));
    rows.push(steps, go);
  }
  if (sold && sold.key === key) rows.push(line(fill(COMPANY_COPY.sold, { o: sold.ore, c: sold.ore * SELL_ORE_CREDITS })));
  const n = view.notice();
  if (n && n.action === 'sell' && n.kind === 'ore') rows.push(line(COMPANY_COPY.oreShort));
  return rows;
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
  // BP-18b-1: a Section an op holds is led by that op's Commander. BP-18b-2b (R9): any other by the Commander its
  // start would send (demo_ops.js's mirror), else the campaign's when none is free.
  const leaderId = view.leaderOf(s.section_id);
  const r = roster();
  const free = leaderId ? null : view.freeLead();
  const c = (leaderId && r && Array.isArray(r.commanders) ? r.commanders.find(x => x.commander_id === leaderId) : null) || (free) || campaignCommander();
  const band = c ? bandWord(c.band) : ''; // BP-19c
  if (c) rows.push(line(fill(band ? COMPANY_COPY.ledBand : COMPANY_COPY.led, { name: c.name, xp: xpLine(c.xp, c.rank), n: c.stress, band }), true));
  const k = view.kit(s.section_id); // BP-23b
  if (k) {
    rows.push(line(fill(COMPANY_COPY.kitHeld, { item: k.item_name, grade: k.quality_grade, p: k.points })));
    rows.push(act(COMPANY_COPY.kitOff, view.held() || view.sectionOut(s.section_id), () => view.equip(s.section_id, null, 'section')));
    const kn = noticeLine('section', 'equip');
    if (kn) rows.push(line(kn));
  } else {
    rows.push(line(COMPANY_COPY.kit, true));
  }
  rows.push(...bankRows('section:' + s.section_id)); // BP-28b
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

// BP-19c: the Foreman on the Crew card: name and Specialisation, rank and XP, stress and band. With a band (a 0.3.3
// server, which also has the bench and the band's heat effects): what the band does to the heat limit, the rank's line
// (none when Shattered, as foreman_threshold), then the rest line, or for a Shattered Foreman the demo's recovery line.
// The recovery line is the Foreman's alone (ODQ-73 is a demo exemption): no Commander surface reads it.
function foremanRows(f) {
  const band = bandWord(f.band);
  const rows = [
    line(fill(COMPANY_COPY.crewForeman, { name: f.name, spec: f.specialization }), true),
    line(fill(COMPANY_COPY.foremanRank, { r: f.rank, xp: xpLine(f.xp, f.rank) })),
    line(fill(band ? COMPANY_COPY.foremanStressBand : COMPANY_COPY.foremanStress, { n: f.stress, band })),
  ];
  if (!band) return rows;
  const shattered = f.band === 'shattered';
  if (FOREMAN_BAND_LINES[f.band]) rows.push(line(FOREMAN_BAND_LINES[f.band], true));
  if (!shattered && FOREMAN_RANK_LINES[f.rank]) rows.push(line(FOREMAN_RANK_LINES[f.rank], true));
  rows.push(line(shattered ? COMPANY_COPY.foremanRecovers : COMPANY_COPY.restNote, true));
  return rows;
}

function crewRows(w, r) {
  const rows = [line(fill(COMPANY_COPY.crewMiners, { n: w.headcount })), line(fill(COMPANY_COPY.crewMorale, { m: w.morale }))];
  const f = Array.isArray(r.foremen) ? r.foremen[0] : null;
  if (f) rows.push(...foremanRows(f)); // BP-19c
  if (!view || !view.adopted()) return rows;
  rows.push(...bankRows('crew:' + w.crew_id)); // BP-28b
  const h = Math.min(8, Math.max(0, Number(w.headcount) || 0));
  return rows.concat(replaceRows('crew', w.crew_id, h, REPLACE_MINER_CREDITS, COMPANY_COPY.hire, COMPANY_COPY.hireNote));
}

// A5: last shelved first (the body lists it last; kit taken off goes to the back, BP-23a), each with its grade and its line.
// BP-23b: an Equip per Section (named when there are two, as the Company list names them), disabled while that
// Section is out or a call is in flight; the server's 409 stays the authority.
function shelfRows() {
  const items = Array.isArray(view.trophies()) ? view.trophies().slice().reverse() : [];
  const rows = [line(COMPANY_COPY.shelfSub, true)];
  const said = noticeLine('shelf', 'equip'); // BP-23b: an equip's refusal, after its re-read
  if (items.length === 0) return rows.concat(line(COMPANY_COPY.shelfEmpty), said ? [line(said)] : []);
  const r = roster();
  const secs = r && Array.isArray(r.sections) ? r.sections : [];
  const isHeld = view.held();
  const texts = secs.some(s => view.sectionOut(s.section_id)) ? [COMPANY_COPY.kitAtBase] : [];
  if (said && !texts.includes(said)) texts.push(said);
  rows.push(...texts.map(t => line(t)));
  for (const t of items) {
    const row = el('div', 'company-trophy');
    row.append(el('p', 'company-trophy-name', String(t.item_name)), el('p', 'company-trophy-grade', String(t.quality_grade)),
      el('p', 'company-trophy-line', TROPHY_LINES.get(t.item_name) || TROPHY_UNKNOWN));
    if (secs.length) {
      const pick = el('div', 'company-equip');
      for (const s of secs) pick.append(act(secs.length === 1 ? COMPANY_COPY.equip : fill(COMPANY_COPY.equipTo, { name: s.name }), isHeld || view.sectionOut(s.section_id), () => view.equip(s.section_id, t, 'shelf')));
      row.append(pick);
    }
    rows.push(row);
  }
  return rows;
}

function openCard(kind, index) {
  const r = roster();
  if (!r) return;
  if (!shown || shown.kind !== kind || shown.index !== index) { view.clearNotice(); selling = null; sold = null; } // BP-28b: + the Sell row
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
  drawHire(); // BP-18b-1: the hire block follows held(), the bank and the last refusal too
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
  const s = view.fightSection(); // BP-18b-1: the fought op's Section
  const n = s ? view.squadOf(s) : 0; // the squad sent (BP-12b: the headcount less the sickbay)
  const labels = [...lines.querySelectorAll('li.score-row .score-label')];
  // The roll's own rows say whether the Gunsmith stood (roll_inputs.gunsmith, read once by the server): no base read.
  const gun = labels.some(l => l.textContent === GUNSMITH_ROW);
  const kit = labels.some(l => l.textContent.startsWith(KIT_ROW)); // BP-23b
  for (const label of labels) {
    const key = label.textContent;
    if (key === 'Gear' && !gun && !kit) label.textContent = GEAR_HINT;
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
// (adopted, credits, ore, out, sectionOut, leaderOf, freeLead, fightSection, company, hired, clearHired, hire, held, serverNow,
// bay, bayOf, squadOf, trophies, notice, clearNotice, heal, replace, sell).
export function mountCompany(d, v) {
  deps = d;
  view = v;
  const style = document.createElement('style');
  style.textContent = CSS;
  document.head.append(style);
  deps.$('btn-back').textContent = COMPANY_COPY.hubButton;
  setOnJoined(id => markAsk(id)); // BP-24b: a 201 join (api.js) marks the new run's ask
  // BP-24b: Continue settles the ask, as Not now does, so it is asked once per run (a reload while it shows asks again).
  deps.$('btn-continue').addEventListener('click', () => {
    const camp = store.campaign();
    if (camp) settleAsk(camp.campaign_id);
  }, true);
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
  new MutationObserver(() => { if (screen.hidden) { fromHub = false; companyUp = false; hideCard(); view.clearHired(); } }).observe(screen, { attributes: true, attributeFilter: ['hidden'] });
  new MutationObserver(() => dressReveal(reveal)).observe(reveal, { childList: true, subtree: true });
  reveal.addEventListener('click', chipAction);
  reveal.addEventListener('keydown', chipAction);
  const aar = deps.$('aar-commander');
  new MutationObserver(() => dressAar(aar)).observe(aar, { childList: true });
  const lines = deps.$('battle-lines');
  new MutationObserver(() => dressScore(lines)).observe(lines, { childList: true });
}
