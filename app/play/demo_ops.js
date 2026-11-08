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
// BP-10b: the Base button opens demo_base.js's sheet, where banked ore buys the five buildings
// (POST ops/build, one per tap). Nothing can be built while any op is out, so the sheet says so
// rather than offering a build the server would refuse (Reviewer BP-10a CF2). BP-21b: a paid
// building goes up over the server's seconds, one at a time: the sheet counts down to the body's
// construction.done_at, and one read at that moment (on the hub, sheet open or not) stands it.
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
//
// BP-18b-2a: every op out is drawn (a marker each) and read through opsOut(); the strip shows one, the
// shown op (shownKey: 'dig' or a contract's section_id), and Other squad switches it when two or more are
// out. Every strip route names the shown op. A contract's fight latches per op (wonBy), and an op whose
// Commander can't lead offers no Fight.
//
// BP-18b-2b: with two Sections and no op out, a start's Go or Guarded opens a squad picker. With an op out, Other
// squad also reaches a Section at home (shownKey 'home:<section_id>', under which shownOp() is null), whose start
// sheet sends it. Every such sheet names the Commander the start sends (the server's prefer_free, mirrored).

import { request, ApiError, store, getMissions, getRoster } from './api.js';
import { onMapCreated } from './hexmap.js';
import { setAarNote, setBeforeOffer, setSquadSize, setDeployExtra, setStill, setFightSquad } from './battle.js';
import { mountStory, writeAar } from './demo_story.js';
import { showCard, cardSeen } from './demo_card.js';
import { mountShop, openShop } from './demo_shop.js'; // BP-25
import { mountReview, countContract } from './demo_review.js'; // BP-27
import { mountCompany, dressBriefing, renderCompany, redrawCompany, healPrice, wageLine, SELL_ORE_CREDITS } from './demo_company.js';
import { DIG_COPY, DIG_CSS, createDigPanel, renderDig, hideDig, homewardLines, flash, bankedLine, crewFightLine } from './demo_dig.js';
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
  goGuarded: 'Guarded',   // BP-17b: a dig's start with the Section (the body as before)
  goAlone: 'Crew alone',  // BP-17b: a dig's start without it (escort: false)
  guard: 'Send the guard',
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
  noCrew: 'No Crew left to dig. Hire one at Your Company, or take a contract.',
  pick: 'Tap a site for the next job.',
  hop: '{n} hexes from here',
  hopSecs: '{s} s on foot',
  leaveOre: 'Leave {ore} ore in the ground?',
  leaveIt: 'Leave it',
  stay: 'Stay',
  listTitle: 'Contracts on offer',
  listSub: 'Pick a contract. The squad walks there first.',
  listOut: 'A squad is out.',
  listMeta: '{n} hexes · {s} s',
  listTag: ' · {word}',              // BP-29a: the difficulty word after a contract row's distance
  briefingTag: 'Difficulty: {word}', // BP-29a: on a contract's briefing (never the raid's or the ambush's)
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
  // BP-18b-1 (Wiki copy)
  squadEmpty: 'That squad has nobody left to send.',
  escortEmpty: 'Nobody left in that squad to stand guard.',
  takesCommand: '{name} is gone. {next} takes command.',
  // BP-18b-2a (Wiki copy)
  other: 'Other squad',
  outTwo: 'Out: {a} at {x}, {b} at {y}.',
  outCrew: 'the Crew',
  noLead: "{name} can't lead this fight. Another Commander takes over when one is free.",
  taken: 'The other squad holds that contract.',
  // BP-18b-2b (Wiki copy)
  leads: 'Leads: {name}',
  noneFree: 'No Commander is free to lead.',
  atBase: '{name} is at base. Tap a site to send them.',
  digHeld: 'The Crew is already at a dig. One dig at a time.',
  outOne: 'Out: {a} at {x}.',
  outThree: 'Out: {a} at {x}, {b} at {y}, {c} at {z}.',
  pickFit: '{name} · {n} fit',
  pickEmpty: '{name} · no soldiers',
  // BP-20b (Wiki copy): the ways and the trip report. A way's warning is the server's own word list, verbatim.
  travelWays: 'Two ways there. Home retraces the way you pick.',
  wayFast: 'Fast',
  waySafe: 'Safe',
  way: '{way} · {s} s',
  wayWarn: '{way} · {s} s · {warning}',
  tripClean: '{title}: the trip went clean. Nobody is sure who to thank.',
  tripHit: '{title}: {what}. {cost}.',
  tripHarmless: 'No harm done',
  hazard: { gas: 'toxic gas', crust: 'the crust gave way', ruins: 'falling masonry' },
  costWounded: '{n} wounded',
  costStress: '{who} stress up {n}',
  costMorale: 'Crew morale down {n}',
  costOre: '{n} ore lost',
  whoCommander: 'Commander',
  whoForeman: 'Foreman',
};

// Refusals the client keys on by exact text (routes/ops.rs; a server reword changes this block in the same brick).
const REFUSAL = {
  short: 'not enough banked credits',
  oreShort: 'not enough banked ore', // BP-28b: ops/sell's store refusal (routes/ops.rs, BP-28a)
  out: 'the team is out: replacements report at base',
  sectionFull: 'no room: a Section holds 8',
  crewFull: 'no room: a Crew holds 8',
  // BP-18b-1 (Reviewer BP-18a-2 F6): matched by equality only; escortEmpty is a prefix of NO_SOLDIERS.
  squadEmpty: 'that squad has no soldiers left',
  escortEmpty: 'no soldiers left to send',
  cap: 'you already have two squads',
  taken: 'another squad has that contract', // BP-18b-2a: Next's pick on a site another op holds
  kitOut: 'the team is out: kit is issued at base', // BP-23b: equip's three 409s (routes/ops.rs:1504-1506)
  notOnShelf: "that item isn't on the Trophy Shelf",
  shelfFull: 'the Trophy Shelf is full',
};

// A12: the relic contract reads as a relic dig (Wiki copy, BP-11b-2). The server's name stays.
const RELIC = {
  mission_id: 'CRASHED_SHIP_RAID',
  name: 'Relic Dig: Crashed Frigate',
  lead: 'A pre-collapse frigate went into the dirt in Sector 6 before anyone now alive was born. The hold is under three metres of soil and a great deal of competitive interest. Dig it out, see off whoever else turns up, and bring back anything that still hums.',
};
const RELIC_SITE = 'contract:CRASHED_SHIP_RAID';
// BP-29a (Wiki, feedback item 9): each contract's difficulty word, by mission type, set from the spec check's 7,200-fight
// runs on the starter roster (wins and casualties both). Crashed Ship wins 95% (GhostWire's Extraction bonus) but wounds
// like a raid: Risky, not Brutal. Never the server's difficulty number, which misleads here; a table, so no fetch and no
// field the store can lose.
const DIFFICULTY = { GUARD_DUTY: 'Easy', SUPPLY_RAID: 'Risky', CRASHED_SHIP_RAID: 'Risky', BLACK_SITE_RAID: 'Brutal' };
const relicWon = o => Boolean(o && o.site_id === RELIC_SITE && o.fought === 'won');
// B4 briefings (BP-15a, Wiki copy): a contract's own briefing over the server's flavour text. The relic keeps
// RELIC.lead; the raid and the ambush keep DIG_COPY's leads, which carry the site and the ore at stake.
const BRIEFING = {
  GUARD_DUTY: 'A Capitalist Bloc drill rig needs minding while it chews through the slag. The dust on the horizon is raiders, and they are not here to talk. Hold the line and the Bloc pays in full. Lose the rig and the Bloc remembers your name, which is worse.',
  SUPPLY_RAID: 'A Socialist Union supply convoy has stopped for the night outside a depot. A Capitalist Bloc procurement office wants the cargo and would rather not have been involved. Get in, get the crates, get paid. The guard is on his fourth coffee and has not looked up once.',
  BLACK_SITE_RAID: 'Nobody is supposed to know this facility exists, so the client paid extra not to be named. The guards were promised a quiet posting and are dug in behind a great deal of concrete to keep it that way. Bring back the files. Do not read the files.',
};
// B4 scenes (BP-15a): one briefing and one fight picture per family, a defeat and a wipeout. A win keeps the keeper's
// victory still, and a picture that fails to load falls back to the keeper's (battle.js stillFailed). The art is
// cached per file name outside the shell, so a re-encoded scene gets a new name, never the old one.
const SCENE = {
  defence: { briefing: 'assets/art/scene_defence_briefing.jpg', battle: 'assets/art/scene_defence_fight.jpg' },
  assault: { briefing: 'assets/art/scene_assault_briefing.jpg', battle: 'assets/art/scene_assault_fight.jpg' },
  expedition: { briefing: 'assets/art/scene_expedition_briefing.jpg', battle: 'assets/art/scene_expedition_fight.jpg' },
  ambush: { briefing: 'assets/art/scene_ambush_fight.jpg', battle: 'assets/art/scene_ambush_fight.jpg' }, // no ambush briefing art
};
const AAR_SCENE = { Defeat: 'assets/art/scene_defeat.jpg', Wipeout: 'assets/art/scene_wipeout.jpg' };
const FAMILY = { GUARD_DUTY: 'defence', SUPPLY_RAID: 'assault', BLACK_SITE_RAID: 'assault', CRASHED_SHIP_RAID: 'expedition' };
// The picture for a keeper screen: the AAR's by outcome, the briefing's and the fight's by the demo fight's own family
// (three fights send GUARD_DUTY, so the family is set where the fight is opened, never read from mission_id).
const sceneFor = (phase, { report }) => {
  if (phase === 'aar') return AAR_SCENE[report && report.outcome] || null;
  const family = opFight && fightDress ? fightDress.family : null;
  return family && SCENE[family] ? SCENE[family][phase] || null : null;
};

// The end of run when the server says nobody is left to send (BP-11b-1).
const END = {
  soldiers: { title: 'No soldiers left', body: 'Every Section is gone to the last Rookie. Command has closed the file and opened a new one.' },
  commander: { title: 'No one left to lead', body: 'There is no Commander left to sign the orders. The Company is already hiring.' },
};
const NO_SOLDIERS = 'no soldiers left to send: start a new run';
const NO_COMMANDER = 'no Commander left to lead: start a new run';
const NO_CREW = 'no Crew left to dig: start a new run';

const REFRESH_READ_MS = 46000; // the server's DEMO_SITE_REFRESH_SECS 45 + 1
const BUILD_READ_SLACK_MS = 250; // BP-21b: the completion read goes out this long after done_at (the server stands it at or after)
const BUILD_REREAD_MS = 1000;    // BP-21b (Reviewer BP-21a F5): the one re-read's least delay, for a body after done_at that still names it

const COLOUR = { dig: '#ff8c2a', ring: '#ffd27a', contract: '#6fd3ff', edge: '#0b0b0c', marker: '#ffffff', route: 'rgba(255, 255, 255, 0.7)', preview: 'rgba(255, 210, 122, 0.8)' };

const CSS = `
.ops-strip { position: absolute; left: 3cqw; right: 3cqw; bottom: 17cqw; padding: 3cqw; display: flex; flex-direction: column; gap: 1.5cqw; text-align: left; background: rgba(11, 11, 12, 0.94); border: 0.4cqw solid var(--panel-edge); border-radius: 2cqw; }
.ops-strip[hidden], .ops-title[hidden], .ops-line[hidden], .ops-actions[hidden], .ops-actions .btn[hidden] { display: none; }
.ops-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 5.5cqw; line-height: 1.1; color: var(--accent); }
.ops-line { margin: 0; font-size: 3.9cqw; line-height: 1.35; }
.ops-actions { display: flex; gap: 2cqw; margin-top: 1cqw; }
.ops-actions .btn { flex: 1; width: auto; max-width: none; min-height: 11cqw; padding: 0 2cqw; font-size: 4.4cqw; }
.ops-ways { display: flex; gap: 2cqw; }
.ops-ways[hidden], .ops-ways .btn[hidden] { display: none; }
.ops-ways .btn { flex: 1; width: auto; max-width: none; min-height: 11cqw; padding: 0 2cqw; font-size: 4cqw; }
.ops-list[hidden] { display: none; }
.ops-list-rows { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 1.5cqw; }
.ops-list-row { width: 100%; max-width: none; display: flex; flex-direction: column; align-items: flex-start; gap: 0.6cqw; padding: 2cqw 2.5cqw; text-align: left; }
.ops-list-name { font-family: var(--display); font-weight: 800; font-size: 4.4cqw; }
.ops-list-meta { font-family: var(--mono); font-size: 3.4cqw; color: var(--muted); }
.briefing-fit[hidden] { display: none; }
.briefing-fit { margin: 0; font-size: 3.8cqw; line-height: 1.35; color: var(--accent); }
.ops-list-tag { color: var(--accent); } /* BP-29a */
.briefing-tag { color: var(--accent); } /* BP-29a */
.briefing-tag[hidden] { display: none; }
.ops-shop { position: absolute; top: 1.5cqw; left: 3cqw; padding: 1cqw 2.5cqw; font-family: var(--mono); font-size: 3.6cqw; color: var(--accent); background: rgba(11, 11, 12, 0.94); border: 0.3cqw solid var(--accent); border-radius: 1.5cqw; cursor: pointer; }
#screen-briefing .briefing-stance[hidden], #screen-briefing #btn-deploy[hidden] { display: none; }
#screen-briefing { gap: 2cqw; }
#screen-briefing .still { height: 24cqw; }
#screen-briefing .briefing-text { font-size: 3.9cqw; line-height: 1.3; }
#screen-briefing .briefing-line { font-size: 3.9cqw; }
#screen-briefing .briefing-traits { font-size: 3.4cqw; line-height: 1.3; }
#screen-briefing .briefing-actions { gap: 1.5cqw; }
#screen-briefing #btn-briefing-back { min-height: 11cqw; padding: 2cqw 6cqw; font-size: 4.8cqw; }
#screen-briefing .briefing-stance { margin: 0; font-family: var(--display); font-weight: 700; font-size: 4.6cqw; line-height: 1.25; color: var(--accent); }
#screen-briefing .briefing-rule { margin: 0; font-size: 3.5cqw; line-height: 1.3; color: var(--muted); }
#screen-briefing .briefing-approaches { display: flex; gap: 2cqw; width: 100%; max-width: 84cqw; }
#screen-briefing .briefing-approaches .btn { flex: 1; width: auto; max-width: none; min-height: 11cqw; padding: 0 2cqw; font-size: 4.4cqw; display: inline-flex; align-items: center; justify-content: center; gap: 1cqw; }
#screen-briefing .briefing-approaches .briefing-icon { flex: none; width: 5cqw; height: 5cqw; }
#screen-battle .still { height: 24cqw; }
#screen-battle .score-lines { gap: 0.8cqw; }
#screen-battle .score-row { padding: 0.8cqw 3cqw; }
#screen-battle .score-rule { margin-top: 0.8cqw; border-top: 0.4cqw solid var(--accent); }
#screen-battle .score-modifier .score-label { color: var(--text); }
#screen-battle .score-note { font-size: 3.6cqw; color: var(--muted); background: none; }
#screen-battle:has(#battle-lines > li:nth-child(10)) .still { height: 16cqw; }
#screen-battle:has(#battle-lines > li:nth-child(10)) .score-modifier .score-label { font-size: 3.8cqw; line-height: 1.2; }
.still[src$="scene_defence_briefing.jpg"] { object-position: 50% 70%; }
.still[src$="scene_defence_fight.jpg"] { object-position: 50% 45%; }
.still[src$="scene_assault_briefing.jpg"] { object-position: 50% 80%; }
.still[src$="scene_assault_fight.jpg"] { object-position: 50% 40%; }
.still[src$="scene_expedition_briefing.jpg"] { object-position: 50% 68%; }
.still[src$="scene_expedition_fight.jpg"] { object-position: 50% 46%; }
.still[src$="scene_ambush_fight.jpg"] { object-position: 50% 37%; }
.still[src$="scene_defeat.jpg"] { object-position: 50% 62%; }
.still[src$="scene_wipeout.jpg"] { object-position: 50% 28%; }
.ops-banked { white-space: pre-line; }
`;

const opsPath = id => '/api/campaign/' + encodeURIComponent(id) + '/ops';
const walkKey = id => 'mm.demo.walk.' + id;
const fill = (text, values) => text.replace(/\{(\w+)\}/g, (m, k) => (k in values ? String(values[k]) : m));

// B5 on the briefing (BP-14b): the stance hint, the rule and the three approaches. The wire values are
// demo_roll.rs MissionApproach's, byte for byte. The client computes no row: the battle screen draws the server's.
const STANCE_LINES = {
  DugIn: 'Intel: the enemy is dug in.',
  Aggressive: 'Intel: the enemy is on the attack.',
  SpreadOut: 'Intel: the enemy is spread out.',
};
const APPROACH_RULE = 'Flank beats dug in. Dig in beats an attack. Assault beats spread out. Your pick deploys.';
const APPROACHES = [['Assault', 'Assault'], ['Flank', 'Flank'], ['DigIn', 'Dig in']]; // [wire value, button label]
const APPROACH_ICON = { Assault: 'icon_approach_assault.png', Flank: 'icon_approach_flank.png', DigIn: 'icon_approach_dig_in.png' }; // BP-22
const VERDICT = {
  good: 'Good call: {p} on the odds.',
  bad: 'Bad call: {p} on the odds.',
  even: 'The enemy hardly noticed your approach.',
};
let pendingApproach = null; // set only inside one approach button's click dispatch (deploy() builds its body synchronously)
let sentApproach = null;    // what the last mission POST carried, recorded where the body is built
let approachButtons = [];   // the three approach buttons, created once at the mount
let stanceNode = null;      // the briefing's stance hint (hidden when the cached body has none)
let fightMission = null;    // the mission id of the demo fight on the briefing, for its stance
// The cached /api/missions body's stance for a mission, or null (a body cached before BP-14a has none: no hint).
const stanceLine = id => { const m = (store.missions() || {})[id]; return m && STANCE_LINES[m.stance] ? STANCE_LINES[m.stance] : null; };
// The AAR's verdict on the pick, keyed only on the server's approach row (absent: the pick was a draw).
const verdictLine = report => {
  if (!sentApproach) return null;
  const row = ((report && report.score_breakdown) || {}).modifiers?.find(m => m.key === 'approach');
  if (!row) return VERDICT.even;
  return fill(row.points > 0 ? VERDICT.good : VERDICT.bad, { p: row.points > 0 ? `+${row.points}` : String(row.points) });
};
// The three buttons follow #btn-deploy's disabled state (every keeper and demo rule, present and future).
const mirrorDeploy = () => { const off = deps.$('btn-deploy').disabled; for (const b of approachButtons) b.disabled = off; };

let deps = null;
let map = null;         // the map the pins are drawn on
let campaignId = null;  // the campaign that map belongs to
let sites = [];
let gateway = null;
let op = null;          // the server's active_op, kept until a homeward leg arrives
let lastBody = null;    // BP-18b-2a: the last adopted body, for opsOut()
let shownKey = null;    // BP-18b-2a: the op the strip shows, by opKey; BP-18b-2b: or 'home:<section_id>', a Section at home
let picker = false;     // BP-18b-2b: the squad picker is up on the open sheet
let prevOps = null;     // BP-18b-2a: the last body's ops by opKey, { kind, leg, fought }, for the per-op latch
let wonBy = new Map();  // BP-18b-2a: the pay each latched contract reports, by opKey
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
  return b.healing > 0 || credits >= healPrice(base).each * b.wounded ? 0 : h;
};
let adoptedFor = null;  // the campaign whose ops body has been adopted (the Company screen needs one, BP-12b)
let companyAsk = null;  // the heal, replace, hire or equip in flight: { action, target }
let companyNotice = null; // its refusal's line on the card: { action, target, kind }
let fightDress = null;  // the demo fight's own briefing dressing, { name, lead, family, tag }, re-applied on every show (Reviewer BP-11b-2 F2)
let fitNote = null;     // the briefing's 0-fit line
let tagNote = null;     // BP-29a: the briefing's difficulty line (a contract's only)
let bankNote = null;    // the one-off "ore banked" line after a laden leg lands
let crewNote = null;    // BP-17b: the one-off wiped line after a Crew wipeout clears the op
let crewFight = null;   // BP-17b: the last adopted body's crew_fight, sticky until the next start
let crewKept = null;    // its JSON, so a new Crew loss re-reads the roster once (spec check F1)
let digKept = null;    // BP-19c: whether the last adopted body held a dig, from its raw ops (null before the first)
let rosterDue = false; // BP-19c: a roster reread owed at the next adopt (campaign entry; the hub after an AAR, op out)
let aarSeen = false;   // BP-19c: an AAR has shown since the hub was last shown
let guardTimer = null;  // BP-17b: the arrived dig's guard countdown (spec check F4)
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
let justBuilt = null;   // BP-21b: the building the last adopted body showed standing for the first time, for its moment on the sheet
let justPaid = null;    // BP-21b: the building the last adopted build POST paid for, for the sheet's pop
let construction = null; // BP-21b: the last adopted body's construction ({ building, done_at }), null when none
let buildKept = null;   // BP-21b: the construction of the body before it, so a building standing is seen as a transition (Reviewer BP-21a F2)
let readArmed = null;   // BP-21b: the done_at the completion read is armed for (once per done_at)
let readTimer = null;   // BP-21b: that read's timer
let rereadFor = null;   // BP-21b (Reviewer BP-21a F5): the done_at that has had its one re-read
let baseTick = null;    // BP-21b: the base sheet's countdown redraw (display only, no read)
let titles = new Map(); // site_id → { title, mission_id }, from every adopted board, kept for the campaign
let walk = null;        // { depart_at, q, r }: where the leg leaving at depart_at set out from (after a Next)
let ways = new Map();   // BP-20b: "q,r" → the board's ways to that hex ({ route, secs, path, warning? }), kept for the campaign
let way = 'fast';       // BP-20b: the way picked on the open sheet; back to 'fast' whenever a sheet opens on another site
let waySite = null;     // BP-20b: the site_id that pick belongs to
let tripNote = null;    // BP-20b: the trip report's lines (one per unseen report, at most 2), until Close
let tripPending = null; // BP-20b: { seq, site_id } of the newest report behind tripNote, written as seen when shown
let tripSeen = null;    // BP-20b (Reviewer BP-20a-2 F1): { seq, site_id } of the newest report shown, mirrored in sessionStorage
let tripTop = null;     // BP-20b: "seq:site_id" of the newest report in the last adopted body (the roster trigger's latch)
let nextFrom = null;    // the team's hex while an ops/next is in flight
let picking = false;    // Next mission: choosing the next site
let pickSite = null;    // the site chosen for Next
let confirming = false; // the ore-on-site confirm is showing
let opFight = false;    // the fight on the briefing was opened from here (the AAR note's guard)
let reopen = null;      // the opener that showed the demo fight's briefing: fight or fightContract (Reviewer BP-12b F2)
let fitTimer = null;    // the 0-fit briefing's re-dress at healed_at (Reviewer BP-12b F2)
let refreshDue = 0;     // when the one refresh read after a contract latch is due, in browser ms
let fightHead = null; // the fought Section's headcount when the demo opened the fight (Reviewer BP-11b-1 F1)
let fightPick = null; // BP-18b-1: the fought op's squad, { commander_id, section_id }, latched when its fight opens (battle.js's fight-squad hook)
let hiredNote = false; // BP-18b-1: a squad hire landed; the Company screen says so until it closes
let endOnHub = null;   // a Section-at-0 end of run waiting for the hub, so it never cuts an AAR (Reviewer BP-11b-1 F1)
let listOpen = false;   // the contracts list is showing

let strip = null, titleNode = null, lineNodes = [], outNode = null, actions = null, buttons = [];
let waysRow = null, wayButtons = []; // BP-20b: the ways row and its two buttons
let dig = null, bankChip = null, endNote = null, baseButton = null, baseSheet = null;
let list = null;
const handlers = [null, null, null, null];
const DEBUG = new URLSearchParams(location.search).get('debug') === '1'; // the markers' count on the canvas, ?debug=1 only

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
// BP-17b: a dig only once its guard stands (spec check F5).
const canNext = o => Boolean(o && o.leg === 'outbound' && progress(o) >= 1 && !battleOf(o) && (o.kind === 'dig' ? guardStands(o) : o.fought));
// BP-17b, the escort: escort_at null = the Crew is alone; the guard stands once the server's clock reaches it.
// Reviewer BP-17b F1: an absent escort_at (a body from before BP-17a) is guarded.
const guardStands = o => Boolean(o && o.dig && (o.dig.escort_at === undefined || (o.dig.escort_at !== null && serverNow() >= Date.parse(o.dig.escort_at))));
// "Go guard the dig": outbound and unguarded, and never with the Section a bare escort sends at 0 (spec check F6).
const canGuard = o => Boolean(o && o.leg === 'outbound' && o.dig && o.dig.escort_at === null && guardHead() !== 0);
// BP-18b-1: the squad an op fights with, from its own keys (H1); an op with neither (an old body) has none.
const squadOfOp = o => (o && (o.section_id || o.commander_id) ? { commander_id: o.commander_id || null, section_id: o.section_id || null } : null);
// BP-18b-2a: a homeward leg home with nothing to bank (the server clears it on its next read).
const homeClear = o => o.leg === 'homeward' && !(o.dig && o.dig.ambush) && progress(o) >= 1 && !(o.dig && o.dig.carried > 0);
// BP-18b-2a: every op out, from the last body: active_op, then the others; the one reader of the others.
const opsOut = () => [op, ...((lastBody && lastBody.more_ops) || []).map(m => m.op)].filter(o => o && !homeClear(o));
// BP-19c (spec check F3): whether a body holds a dig, from its raw ops. Never opsOut(): homeClear drops a carried-0
// arrival on the client before the server has cleared it; only a body the server built shows the slot cleared.
const digIn = b => [b.active_op, ...(b.more_ops || []).map(m => m && m.op)].some(o => Boolean(o && o.kind === 'dig'));
// An op's key: the dig is 'dig', a contract its Section.
const opKey = o => (o.kind === 'dig' ? 'dig' : o.section_id || null);
// BP-18b-2b: the Section a home key names, else null.
const homeKey = () => (typeof shownKey === 'string' && shownKey.startsWith('home:') ? shownKey.slice(5) : null);
// The op the strip shows: the one shownKey names, else active_op (the first out); none under a home key (R2).
const shownOp = () => { if (homeKey() !== null) return null; const all = opsOut(); return all.find(o => opKey(o) === shownKey) || all[0] || null; };
// What the strip shows of an op mid-leg: how many ops are out (Other squad and the out line follow it), which op,
// its leg and whether it has arrived (`p`: its progress).
const phaseOf = (o, p) => `${opsOut().length}:${opKey(o)}:${o.leg}:${p >= 1}`;
// BP-18b-2b: the roster's Sections and Commanders (main.js's kept roster), in roster order.
const rosterSections = () => { const r = deps.getRoster(); return r && Array.isArray(r.sections) ? r.sections : []; };
const rosterCommanders = () => { const r = deps.getRoster(); return r && Array.isArray(r.commanders) ? r.commanders : []; };
// BP-18b-2b (R2): the Sections at home: soldiers, and no op out carries them.
function homeSquads() {
  const held = new Set(opsOut().map(o => o.section_id).filter(Boolean));
  return rosterSections().filter(s => (s.headcount > 0) && (!held.has(s.section_id)));
}
// BP-18b-2b (R2, Linux H4): Other squad shows with two ops out, or with one op out and a Section at home of two or more.
const canOther = () => opsOut().length >= 2 || ((rosterSections().length >= 2) && homeSquads().length >= 1 && (opsOut().length >= 1));
// The home phase draw() compares (R4): an op landing home redraws the out line and Other squad.
const homePhase = () => `home:${opsOut().length}:${homeSquads().length}`;
// "Other squad" ends every strip of an op while canOther() holds (R5 of BP-18b-2a; R2).
const withOther = acts => (canOther() ? [...acts, [COPY.other, otherSquad]] : acts);
// BP-18b-2b (R8, Reviewer BP-18b-1 F3): the Commander a named start sends, as the server's prefer_free picks it: the
// first usable one (unusableLead's test) that no op out carries; null when none is free (a named start then answers
// 409 every Commander is out).
function freeLead() {
  const busy = new Set(opsOut().map(o => o.commander_id).filter(Boolean));
  return rosterCommanders().find(c => !c.is_kia && !c.is_shattered && !busy.has(c.commander_id)) || null; // BP-19c: the flag alone (spec check 7)
}
// The sheet's first line with two or more Commanders: who leads, or that nobody is free; [] with one.
const leadLines = () => (rosterCommanders().length >= 2 ? [freeLead() ? fill(COPY.leads, { name: freeLead().name }) : COPY.noneFree] : []);
const noneFree = () => rosterCommanders().length >= 2 && !freeLead();
// The roster's Commander an op names, when KIA or Shattered (as battle.js's patchRoster keeps them); else null.
function unusableLead(o) {
  const roster = deps.getRoster();
  const c = o && o.commander_id && roster && Array.isArray(roster.commanders) ? roster.commanders.find(x => x.commander_id === o.commander_id) : null;
  return c && (c.is_kia || c.is_shattered) ? c : null; // BP-19c: the flag alone; patchRoster sets it at 100
}
// The out line: each op's Section name (the Crew for a Crew-alone dig) and its site, in opsOut() order. Two or three
// ops anywhere; one op only under a home key (the home hub and home sheets, BP-18b-2b R5).
function outLine() {
  const all = opsOut();
  const sections = rosterSections();
  const who = o => (o.section_id ? (sections.find(s => s.section_id === o.section_id) || {}).name || '' : COPY.outCrew);
  if (all.length === 1 && homeKey() !== null) return fill(COPY.outOne, { a: who(all[0]), x: opTitle(all[0]) });
  if (all.length === 2) return fill(COPY.outTwo, { a: who(all[0]), x: opTitle(all[0]), b: who(all[1]), y: opTitle(all[1]) });
  if ((all.length === 3)) return fill(COPY.outThree, { a: who(all[0]), x: opTitle(all[0]), b: who(all[1]), y: opTitle(all[1]), c: who(all[2]), z: opTitle(all[2]) });
  return '';
}
// The Crew fight line (spec check F2): every result but a raid held is the op's last fight and shows as is;
// a raid held can be followed by another fight, so it shows only outbound while no guard stands.
const crewShown = o => {
  if (!crewFight) return '';
  const heldRaid = crewFight.kind === 'raid' && crewFight.result === 'held';
  return heldRaid && !(o && o.leg === 'outbound' && !guardStands(o)) ? '' : crewFightLine(crewFight);
};
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

// BP-20b: the way an op walks, from the board's ways at its hex: Safe when the op says so, else Fast (a Next'd op's
// way home is the target's Fast way, as the server walks it). Null when the board never showed that hex's ways.
function wayOf(o) {
  const rs = ways.get(`${o.at.q},${o.at.r}`);
  return rs ? rs.find(w => w.route === (o.route || 'fast')) || null : null;
}

// BP-20b: the hexes an op's leg runs through, in travel order: the gateway then the way's path out, reversed home.
// A Next's hop is straight (the server walks no path there), and so is a leg whose way the board never showed.
function wayPoints(o) {
  const ends = legEnds(o);
  const w = wayOf(o);
  if (!w || !gateway || (o.leg === 'outbound' && ends[0] !== gateway)) return ends;
  const pts = [gateway, ...w.path.map(([q, r]) => ({ q, r }))];
  return o.leg === 'outbound' ? pts : pts.reverse();
}

// BP-20b (F-C1): the point a share p of the way along pts, an equal share per hex (display only: the server's
// seconds per hex follow the terrain). Two points give the straight line exactly as before.
function alongPath(pts, p) {
  const f = Math.min(1, Math.max(0, p)) * (pts.length - 1);
  const i = Math.min(pts.length - 2, Math.floor(f));
  const t = f - i;
  return { q: pts[i].q + (pts[i + 1].q - pts[i].q) * t, r: pts[i].r + (pts[i + 1].r - pts[i].r) * t };
}

// BP-20b: one dashed line through hexes; with two hexes, the route line as before, call for call.
function strokeWay(ctx, m, pts, colour, width) {
  ctx.setLineDash([4, 4]);
  ctx.lineWidth = width;
  ctx.strokeStyle = colour;
  ctx.beginPath();
  pts.forEach((h, i) => {
    const [x, y] = m.hexToScreen(h.q, h.r);
    if (i === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  });
  ctx.stroke();
  ctx.setLineDash([]);
}

// BP-20b: the way an open sheet shows on the map: the picked one on this site, else Fast.
function previewWay(site) {
  const rs = Array.isArray(site.routes) ? site.routes : [];
  return rs.find(w => w.route === (site.site_id === waySite ? way : 'fast')) || null;
}

// BP-20b: a site offering both ways gets the ways row; the pick goes back to Fast when a sheet opens on another site.
function wayChoice(site) {
  if (site.site_id !== waySite) { waySite = site.site_id; way = 'fast'; }
  const rs = Array.isArray(site.routes) ? site.routes : [];
  return rs.length === 2 ? rs : null;
}

// BP-20b: the start body's way. Only a Safe pick on a site still offering Safe sends one; absent is Fast, so every
// other start body is byte-identical to before. Next mission sends none (its hop takes Fast by ruling).
function wayBody(site) {
  const rs = site && Array.isArray(site.routes) ? site.routes : [];
  return way === 'safe' && site.site_id === waySite && rs.some(w => w.route === 'safe') ? { route: 'safe' } : {};
}

// BP-20b: the ways row: a button per way with its seconds and the server's warning, verbatim (never odds or a
// count). The pick is marked, and tapping one redraws the sheet and its way on the map.
function setWays(choice) {
  waysRow.hidden = !choice;
  wayButtons.forEach((b, i) => {
    const w = choice ? choice[i] : null;
    b.hidden = !w;
    if (!w) return;
    const name = w.route === 'safe' ? COPY.waySafe : COPY.wayFast;
    const warn = w.warning;
    b.textContent = warn ? fill(COPY.wayWarn, { way: name, s: Math.round(w.secs), warning: warn }) : fill(COPY.way, { way: name, s: Math.round(w.secs) });
    b.dataset.route = w.route;
    b.classList.toggle('btn-primary', w.route === way);
    b.setAttribute('aria-pressed', String(w.route === way));
    b.disabled = held();
  });
}

// BP-20b (Reviewer BP-20a-2 F1): the shown report's key, per campaign, in sessionStorage like the card's (it
// survives a reload and dies with the tab); tripSeen is its mirror for private mode.
const tripKey = id => `mm.demo.trip.${id}`;
function loadTripSeen(id) {
  try { return JSON.parse(sessionStorage.getItem(tripKey(id)) || 'null'); } catch { return null; }
}

// BP-20b: one line per report: what hit, each kind once in the order met, and what it cost in all, as the server
// sent it. The stress is a Foreman's on a dig and a Commander's on a contract, read from site_id (a fought
// contract may have left the board).
function tripLine(r) {
  const title = opTitle(r);
  const hits = Array.isArray(r.hits) ? r.hits : [];
  if (hits.length === 0) return fill(COPY.tripClean, { title });
  const what = [];
  for (const h of hits) {
    const word = COPY.hazard[h.hazard] || h.hazard;
    if (!what.includes(word)) what.push(word);
  }
  const sum = k => hits.reduce((n, h) => n + (Number(h[k]) || 0), 0);
  const who = r.site_id.startsWith('dig:') ? COPY.whoForeman : COPY.whoCommander;
  const cost = [];
  if (sum('wounded') > 0) cost.push(fill(COPY.costWounded, { n: sum('wounded') }));
  if (sum('stress') > 0) cost.push(fill(COPY.costStress, { who, n: sum('stress') }));
  if (sum('morale') > 0) cost.push(fill(COPY.costMorale, { n: sum('morale') }));
  if (sum('ore') > 0) cost.push(fill(COPY.costOre, { n: sum('ore') }));
  return fill(COPY.tripHit, { title, what: what.join(', '), cost: cost.length > 0 ? cost.join(', ') : COPY.tripHarmless });
}

// BP-20b: the report's lines for the hub strip, written as seen as they are shown (F1: at the showing, never at
// adopt, since a start's 200 can carry a report under a travelling strip).
function tripShown() {
  if (!tripNote) return [];
  if (tripPending) {
    tripSeen = tripPending;
    tripPending = null;
    try { sessionStorage.setItem(tripKey(campaignId), JSON.stringify(tripSeen)); } catch { /* the mirror holds it */ }
  }
  return tripNote;
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

// `lines`: up to four strings (BP-18b-2b: a sheet's Leads line is the fourth node); `acts`: up to four
// [text, onClick, disabled] buttons. The out line follows the lines (outLine decides when, BP-18b-2a/2b).
function setStrip(title, lines, acts, danger, choice) {
  titleNode.hidden = !title;
  titleNode.textContent = title || '';
  titleNode.classList.toggle('danger', Boolean(danger));
  lineNodes.forEach((node, i) => {
    node.hidden = !lines[i];
    node.textContent = lines[i] || '';
  });
  const out = outLine();
  outNode.hidden = !out;
  outNode.textContent = out;
  buttons.forEach((b, i) => {
    const a = acts[i];
    b.hidden = !a;
    handlers[i] = a ? a[1] : null;
    b.textContent = a ? a[0] : '';
    b.disabled = Boolean(a && (a[2] || held()));
  });
  setWays(choice); // BP-20b: the ways row shows only on a start sheet offering both ways
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
  const wages = wageLine(company); // BP-18b-1: the wage line under the bank (pre-line)
  bankChip.textContent = wages ? bankedLine(banked, credits) + '\n' + wages : bankedLine(banked, credits);
  bankChip.hidden = !(banked > 0 || credits > 0);
  shownPhase = null;
  shownSecs = null;
  const rate = Date.now() < throttleUntil ? fill(DIG_COPY.rate, { n: throttleSecs }) : notice;
  baseButton.hidden = !gateway;
  const so = shownOp(); // BP-18b-2a: the strip reads the shown op
  if (baseOpen || listOpen) {
    // A sheet covers the strip; the phase is noted so a leg's frames don't redraw it each time.
    hideStrip();
    hideDig(dig);
    if (so) shownPhase = phaseOf(so, progress(so));
    else if (homeKey() !== null) shownPhase = homePhase(); // BP-18b-2b: no redraw per frame under a sheet
    if (so && progress(so) < 1) shownSecs = secsLeft(so);
    if (baseOpen) {
      list.node.hidden = true;
      renderBase(baseSheet, { base, banked, credits, out: Boolean(op), held: held(), rate, going: goingUp() }, justBuilt, justPaid);
      justBuilt = null;
      justPaid = null;
      armBaseTick();
    } else {
      hideBase(baseSheet);
      renderList();
    }
    return;
  }
  hideBase(baseSheet);
  list.node.hidden = true;
  if (so && picking && canNext(so)) {
    hideDig(dig);
    shownPhase = phaseOf(so, 1);
    return showPick(rate);
  }
  if (so && so.leg === 'outbound' && so.dig && progress(so) >= 1) return showDig(so);
  hideDig(dig);
  if (so) {
    const title = opTitle(so);
    const p = progress(so);
    const lead = unusableLead(so); // BP-18b-2a (F2, R6): nobody can lead its fight, so no Fight
    shownPhase = phaseOf(so, p);
    if (so.leg === 'homeward') {
      if (so.dig && so.dig.ambush) {
        return lead ? setStrip('', [DIG_COPY.ambushWait, fill(COPY.noLead, { name: lead.name }), rate], withOther([]))
          : setStrip('', [DIG_COPY.ambushWait, rate], withOther([[DIG_COPY.fight, fight]]));
      }
      shownSecs = secsLeft(so);
      const h = homewardLines(so.dig, title, shownSecs);
      const c = crewShown(so);
      if (h) return setStrip(h.title, [...h.lines, ...(c ? [c] : []), rate], withOther([]), h.danger);
      return setStrip('', [fill(COPY.homeward, { n: shownSecs }), rate], withOther([]));
    }
    if (p < 1) {
      shownSecs = secsLeft(so);
      return setStrip('', [fill(COPY.outbound, { title, n: shownSecs }), rate], withOther([[COPY.ret, null, true], ...(canGuard(so) ? [[COPY.guard, sendEscort]] : [])]));
    }
    if (so.kind === 'contract') {
      // Fight while unfought (and someone can lead it); Next mission wherever canNext allows it; Head home always.
      const acts = [];
      if (!so.fought && !lead) acts.push([COPY.fight, fightContract]);
      if (canNext(so)) acts.push([COPY.next, startPick]);
      acts.push([DIG_COPY.home, sendReturn]);
      const won = wonBy.get(opKey(so)) || 0;
      const line = !so.fought ? (lead ? fill(COPY.noLead, { name: lead.name }) : fill(COPY.atContract, { title }))
        : so.fought === 'won' ? fill(won > 0 ? COPY.won : COPY.wonNoPay, { title, credits: won }) : fill(COPY.lost, { title });
      return setStrip('', [line, rate], withOther(acts));
    }
    return setStrip('', [fill(COPY.onSite, { title }), rate], withOther([[COPY.ret, sendReturn]]));
  }
  const home = homeKey();
  if (home !== null) shownPhase = homePhase(); // BP-18b-2b (R4): draw redraws a home key's strip or sheet on a change
  if (selected) return showSheet(selected);
  if (home !== null) { // BP-18b-2b (R5): the home hub
    const s = rosterSections().find(x => x.section_id === home);
    return setStrip('', [fill(COPY.atBase, { name: s ? s.name : '' }), rate], withOther([]));
  }
  const trip = tripShown(); // BP-20b: the trip report rides the bank or wipe note, or stands alone
  if (bankNote) return setStrip('', [bankNote, ...trip, rate], [[BASE_COPY.button, openBase], [COPY.close, close]]);
  if (crewNote) return setStrip('', [crewNote, ...trip, rate], [[COPY.close, close]]);
  if (trip.length > 0) return setStrip('', [...trip, rate], [[COPY.close, close]]);
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
  shownPhase = phaseOf(o, 1);
  renderDig(dig, o.dig, store.roster(campaignId), { guard: guardLine(o), crewFight: crewShown(o) });
  armGuard(o);
  // The third slot: Send the guard while the dig is unguarded, else Next mission wherever canNext allows it.
  const next = canGuard(o) ? [[COPY.guard, sendEscort]] : canNext(o) ? [[COPY.next, startPick]] : [];
  if (o.dig.raid) {
    const lead = unusableLead(o); // BP-18b-2a (F2, R6): the raid waits until a Commander is free
    if (lead) return setStrip(title, [fill(DIG_COPY.raidWait, { title }), fill(COPY.noLead, { name: lead.name }), rate], withOther(next));
    return setStrip(title, [fill(DIG_COPY.raidWait, { title }), rate], withOther([[DIG_COPY.fight, fight], ...next]));
  }
  const second = o.dig.ore_on_site > 0 ? [DIG_COPY.pull, sendPull] : [DIG_COPY.home, sendReturn];
  return setStrip(title, [rate], withOther([[DIG_COPY.drill, sendDrill], second, ...next]));
}

// BP-17b: the dig card's guard line: alone, or the Section's countdown to standing guard.
function guardLine(o) {
  if (!o.dig || o.leg !== 'outbound') return '';
  if (o.dig.escort_at === null) return DIG_COPY.unguarded;
  const s = Math.ceil((Date.parse(o.dig.escort_at) - serverNow()) / 1000);
  return s > 0 ? fill(DIG_COPY.guardWalking, { n: s }) : '';
}

// The guard's countdown at an arrived dig (spec check F4): nothing else redraws there, so a display
// timer runs only while escort_at is in the future. An escorted dig (escort_at = depart_at) arms none.
function armGuard(o) {
  if (guardTimer !== null || !o.dig || o.dig.escort_at === null) return;
  const wait = Date.parse(o.dig.escort_at) - serverNow();
  if (!(wait > 0)) return;
  guardTimer = setTimeout(() => { guardTimer = null; refresh(); }, Math.min(1000, wait));
}

// BP-21b: the building going up, for the sheet: { id, left }, whole seconds to done_at on the server's clock, or null.
function goingUp() {
  if (!construction) return null;
  return { id: construction.building, left: Math.max(0, Math.ceil((Date.parse(construction.done_at) - serverNow()) / 1000)) };
}

// BP-21b: nothing else redraws an idle sheet, so while it is open and a building is going up a display timer
// redraws it on each of done_at's second boundaries. It reads nothing; closing the sheet stops it at its next fire.
function armBaseTick() {
  if (baseTick !== null || !baseOpen || !construction) return;
  const wait = Date.parse(construction.done_at) - serverNow();
  if (!(wait > 0)) return;
  baseTick = setTimeout(() => { baseTick = null; refresh(); }, Math.min(1000, wait % 1000 || 1000));
}

// BP-21b: one read when the building going up is due to stand. adopt arms it once per done_at (and once more
// for F5's re-read). It goes out on the hub only, sheet open or not, since the dig panel's raid risk follows a
// Radio Shack too (BP-21a R3 (b)); a held moment or another screen retries every 500 ms (Reviewer BP-11b-1 F1).
function armBuildRead(delay) {
  if (readTimer !== null) clearTimeout(readTimer);
  readTimer = setTimeout(buildTick, Math.max(0, delay));
}

function buildTick() {
  readTimer = null;
  if (!construction || construction.done_at !== readArmed) return;
  if (held() || !campaignId || !hubShown()) { readTimer = setTimeout(buildTick, 500); return; }
  load();
}

function showSheet(site) {
  if (picker) return showPicker(site);
  const lines = [fill(COPY.distance, { n: Math.round(site.distance) }), fill(COPY.travel, { n: Math.round(site.travel_secs) })];
  // BP-18b-2b (R7, Linux H2): one dig at a time, so a dig pin offers no start while a dig is out.
  if (site.kind === 'dig' && (opsOut().some(o => o.kind === 'dig'))) {
    if (COPY.digReward[site.tier]) lines.push(COPY.digReward[site.tier]);
    return setStrip(titleOf(site), [COPY.digHeld, ...lines], withOther([[COPY.close, close]]));
  }
  const two = wayChoice(site); // BP-20b: both ways offered, so their times sit on the way buttons
  if (two) lines[1] = COPY.travelWays;
  // BP-18b-2b (R8): who leads, first, with two or more Commanders; nobody free disables the squad's start.
  const head = leadLines();
  const off = (noneFree());
  if (site.kind === 'dig') {
    if (COPY.digReward[site.tier]) lines.push(COPY.digReward[site.tier]);
  } else {
    // The reward is the mission's, from the cached list; without it the line is left out.
    const at = head.length + lines.length;
    getMissions().then(ms => {
      const m = ms && ms[site.mission_id];
      if (!m || shownOp() || picker || selected !== site) return;
      const text = m.ore_reward > 0
        ? fill(COPY.reward, { credits: m.credit_reward, ore: m.ore_reward })
        : fill(COPY.rewardNoOre, { credits: m.credit_reward });
      lineNodes[at].textContent = text;
      lineNodes[at].hidden = false;
    }, () => {});
  }
  const go = site.kind === 'dig' ? [[COPY.goGuarded, startGo, off], [COPY.goAlone, sendAlone]] : [[COPY.go, startGo, off]];
  setStrip(titleOf(site), [...head, ...lines], withOther([...go, [COPY.close, close]]), false, two);
}

// BP-18b-2b (R6): the picker opens on a start with no op out and two or more Sections.
const pickerOn = () => opsOut().length === 0 && (rosterSections().length >= 2);

// A start sheet's Go or Guarded: a home key sends its Section (R7); else the picker, else today's body.
function startGo() {
  if (!selected) return;
  const home = homeKey();
  if (home !== null) return sendStart(home);
  if (pickerOn()) {
    picker = true;
    return refresh();
  }
  sendStart(null);
}

// The picker: one button per Section in roster order (its fit count, or disabled with no soldiers), then Cancel.
function showPicker(site) {
  const off = noneFree();
  const acts = rosterSections().map(s => (s.headcount > 0
    ? [fill(COPY.pickFit, { name: s.name, n: fitToFight({ section: s }) }), () => sendStart(s.section_id), off]
    : [fill(COPY.pickEmpty, { name: s.name }), null, true]));
  setStrip(titleOf(site), leadLines(), [...acts, [COPY.cancel, cancelPicker]]);
}

function cancelPicker() {
  picker = false;
  refresh();
}

// Next mission's three steps: the prompt, the chosen site's hop, and the ore-on-site confirm.
function showPick(rate) {
  if (!pickSite) return setStrip('', [COPY.pick, rate], [[COPY.cancel, cancelPick]]);
  const so = shownOp();
  if (confirming) {
    return setStrip('', [fill(COPY.leaveOre, { ore: so.dig.ore_on_site }), rate],
      [[COPY.leaveIt, () => sendNext(pickSite)], [COPY.stay, cancelPick]]);
  }
  const hop = hexDistance(so.at, pickSite);
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

// BP-18b-2a: Other squad moves the strip to the next op out, in opsOut() order, wrapping. BP-18b-2b (R2): then to each
// Section at home; leaving a key closes its sheet and picker.
function otherSquad() {
  if (!canOther()) return;
  const keys = [...opsOut().map(opKey), ...homeSquads().map(s => 'home:' + s.section_id)];
  const so = shownOp();
  const i = keys.indexOf(homeKey() !== null ? shownKey : so ? opKey(so) : null);
  shownKey = keys[(i + 1) % keys.length];
  selected = null;
  picker = false;
  picking = false;
  pickSite = null;
  confirming = false;
  if (map) map.requestFrame();
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
  const so = shownOp();
  if (!site || !so || sameHex(site, so.at)) return;
  pickSite = site;
  confirming = false;
  refresh();
}

// Leaving a dig with ore on site walks away from it (BP-11a-4), so that asks first.
function goNext() {
  if (!pickSite) return;
  const so = shownOp();
  if (so && so.dig && so.dig.ore_on_site > 0) {
    confirming = true;
    return refresh();
  }
  sendNext(pickSite);
}

function close() {
  if (map) map.requestFrame(); // BP-20b: the sheet's way leaves the map
  selected = null;
  picker = false;
  noDigOpen = false;
  bankNote = null; crewNote = null; tripNote = null;
  notice = '';
  if (homeKey() !== null) return refresh(); // BP-18b-2b: back to the home hub
  hideStrip();
}

// The Base button toggles the sheet; the bank note's Base opens it.
function openBase() {
  closeLegend();
  baseOpen = true;
  listOpen = false;
  selected = null;
  picker = false;
  noDigOpen = false;
  bankNote = null; crewNote = null; tripNote = null;
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
  picker = false;
  noDigOpen = false;
  bankNote = null; crewNote = null; tripNote = null;
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
  const out = Boolean(shownOp()) && !picking;
  list.sub.textContent = out ? COPY.listOut : COPY.listSub;
  list.rows.replaceChildren();
  for (const site of sites.filter(s => s.kind === 'contract')) {
    const row = el('button', 'btn ops-list-row');
    row.type = 'button';
    row.disabled = out;
    const meta = el('span', 'ops-list-meta', fill(COPY.listMeta, { n: Math.round(site.distance), s: Math.round(site.travel_secs) }));
    const word = DIFFICULTY[site.mission_id]; // BP-29a
    if (word) meta.append(el('span', 'ops-list-tag', fill(COPY.listTag, { word })));
    row.append(el('span', 'ops-list-name', titleOf(site)), meta);
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
  if (shownOp()) {
    if (picking) return choose(site);
    return refresh();
  }
  selected = site;
  if (map) map.requestFrame(); // BP-20b: the sheet's way on the map
  picker = false;
  noDigOpen = false;
  bankNote = null; crewNote = null; tripNote = null;
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
  fightMission = b.mission_id;
  fightPick = squadOfOp(op);
  fightHead = sectionHead();
  fightDress = { name: null, lead, family: op.dig.raid ? 'defence' : 'ambush' };
  await deps.openMission(b.mission_id, () => deps.enterHub());
  if (!deps.$('screen-briefing').hidden) dressFight();
}

// The contract's own briefing; its mission comes from the title cache, never the board. The relic
// contract's name and text are the demo's (A12); every briefing gets the stress and trait lines.
async function fightContract() {
  const so = shownOp();
  const t = so && titles.get(so.site_id);
  if (!t || !t.mission_id) return;
  opFight = true;
  reopen = fightContract;
  fightMission = t.mission_id;
  fightPick = squadOfOp(so);
  fightHead = sectionHead();
  fightDress = t.mission_id === RELIC.mission_id
    ? { name: RELIC.name, lead: RELIC.lead, family: FAMILY[t.mission_id], tag: DIFFICULTY[t.mission_id] || null }
    : { name: null, lead: BRIEFING[t.mission_id] || null, family: FAMILY[t.mission_id] || null, tag: DIFFICULTY[t.mission_id] || null };
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
  if (!tagNote) {
    tagNote = el('p', 'briefing-line briefing-tag');
    deps.$('briefing-name').after(tagNote);
  }
  const tag = fightDress && fightDress.tag ? fightDress.tag : null; // BP-29a: a raid's or an ambush's dressing has none
  tagNote.textContent = tag ? fill(COPY.briefingTag, { word: tag }) : '';
  tagNote.hidden = !tag;
  const section = fightSection();
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
  dressApproach();
}

// B5: on every briefing show, after the keeper's disabled = false (and dressFight's 0-fit rule), the keeper
// Deploy is hidden behind the three approach buttons, the stance hint is set and the buttons follow its state.
function dressApproach() {
  deps.$('btn-deploy').hidden = true;
  const line = opFight && fightMission ? stanceLine(fightMission) : null;
  stanceNode.textContent = line || '';
  stanceNode.hidden = !line;
  mirrorDeploy();
}

// The stance line, the rule and the row of three, above the keeper Deploy (the ?debug=1 checkbox stays above them).
function createApproaches() {
  const deploy = deps.$('btn-deploy');
  stanceNode = el('p', 'briefing-stance');
  stanceNode.hidden = true;
  const rule = el('p', 'briefing-rule', APPROACH_RULE);
  const row = el('div', 'briefing-approaches');
  approachButtons = APPROACHES.map(([wire, label]) => {
    const b = el('button', 'btn btn-primary briefing-approach', label);
    b.type = 'button';
    b.dataset.approach = wire;
    const icon = el('img', 'briefing-icon');
    icon.alt = '';
    icon.addEventListener('error', () => icon.remove());
    icon.src = `assets/art/${APPROACH_ICON[wire]}`;
    b.prepend(icon);
    b.addEventListener('click', () => {
      pendingApproach = wire; deps.$('btn-deploy').click(); pendingApproach = null;
    });
    return b;
  });
  row.append(...approachButtons);
  deploy.before(stanceNode, rule, row);
  new MutationObserver(mirrorDeploy).observe(deploy, { attributes: true, attributeFilter: ['disabled'] });
}

// The campaign Commander's name from main.js's roster, for the card's opening line.
function commanderName() {
  const camp = store.campaign();
  const roster = deps.getRoster();
  const c = camp && roster && Array.isArray(roster.commanders) ? roster.commanders.find(x => x.commander_id === camp.commander_id) : null;
  return c && typeof c.name === 'string' ? c.name : '';
}

// BP-18b-1: the Section the fight sends, as battle.js's deployed() picks it: the fought op's, else the first.
function fightSection() {
  const roster = deps.getRoster();
  const sections = roster && Array.isArray(roster.sections) ? roster.sections : [];
  const id = fightPick && fightPick.section_id;
  return (id ? sections.find(s => s.section_id === id) : sections.find(() => true)) || null;
}

// The fought Section's headcount in main.js's roster, the one battle.js's squad reads.
function sectionHead() {
  const section = fightSection();
  return section ? section.headcount : null;
}

// The headcount of the Section a bare escort sends (the server's pick): the first in roster order with
// soldiers that the op does not hold; 0 when none has any; null with no roster.
function guardHead() {
  const roster = deps.getRoster();
  const sections = roster && Array.isArray(roster.sections) ? roster.sections : [];
  if (sections.length === 0) return null;
  const free = sections.find(s => s.headcount > 0 && !opsOut().some(o => o.section_id === s.section_id)); // every op (F5)
  return free ? free.headcount : 0;
}

// The AAR's loss lines for a fight opened from here: who died, and the Section's count after it
// (the server takes one soldier per KIA or MIA when the fight latches).
function aarNote({ report, section }) {
  writeAar(report, verdictLine(report)); // B4 + B5 (BP-15b): the write-up, then the verdict on the pick, under the banner
  if (!opFight) return [];
  if (reopen === fightContract) countContract(); // BP-27: a contract's AAR; a raid's or an ambush's reopens fight
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
  // BP-18b-1 (R3): the fought Commander is dead or Shattered (battle.js has patched the roster) and another can
  // lead: the next is battle.js's nextUsable pick, the first usable in roster order.
  const roster = deps.getRoster();
  const commanders = roster && Array.isArray(roster.commanders) ? roster.commanders : [];
  const camp = store.campaign();
  const ledId = (fightPick && fightPick.commander_id) || (camp && camp.commander_id);
  const led = commanders.find(c => c.commander_id === ledId);
  const next = commanders.find(c => !c.is_kia && !c.is_shattered);
  if (led && (led.is_kia || led.is_shattered) && next) lines.push(fill(COPY.takesCommand, { name: led.name, next: next.name }));
  return lines;
}

// The run is over: nobody is left to send. The keeper end screen's New run does the rest.
function endOfRun(kind) {
  const c = END[kind];
  deps.$('end-title').textContent = c.title;
  deps.$('end-name').textContent = ''; // BP-18b-1: the soldiers' card names no Section (every one is at 0)
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
  // BP-18b-2a: a route and a marker per op out, the shown op last.
  const shown = shownOp();
  const all = opsOut();
  const order = [...all.filter(o => o !== shown), ...(shown ? [shown] : [])];
  for (const o of order) {
    const pts = gateway ? wayPoints(o) : null;
    if (!pts) continue;
    strokeWay(ctx, m, pts, COLOUR.route, Math.max(1, s * 0.1)); // BP-20b: the way it walks, else the straight line
  }
  // BP-20b: an open sheet shows its way (the picked one) in its own colour, under the pins.
  const pw = !shown && selected && gateway ? previewWay(selected) : null;
  if (pw) strokeWay(ctx, m, [gateway, ...pw.path.map(([q, r]) => ({ q, r }))], COLOUR.preview, Math.max(1, s * 0.1));
  for (const st of sites) {
    const [x, y] = m.hexToScreen(st.q, st.r);
    if (onScreen(x, y)) pin(ctx, st, x, y, r, line);
  }
  // An ambushed team waits at the midpoint until its fight is latched; the leg's times run on
  // meanwhile, so the marker is pinned rather than read from them (Reviewer BP-8b-2 F2).
  const isAmbush = o => Boolean(o.dig && o.dig.ambush);
  const at = o => (isAmbush(o) ? 0.5 : progress(o));
  if (op) {
    const ambush = isAmbush(op);
    const p = at(op);
    const home = !ambush && op.leg === 'homeward' && p >= 1;
    if (home && !(op.dig && op.dig.carried > 0)) {
      // BP-20b (F-D1): a way that crossed a hazard has a trip report waiting, so its landing reads once (op is
      // cleared below, so no later frame sends it again). Skipped while a call is in flight: the next read brings it.
      const w = wayOf(op);
      if (!held() && w && w.warning) load();
      op = null; // home with nothing to bank: the server clears it on its next read
      selected = null;
      refresh();
    } else {
      // Each flag is set only when its read is sent: a read skipped while another call is in flight
      // is sent from the frame after that call clears (Reviewer BP-9b F1). Both stay on the dig.
      if (home && !landAsked && !held()) { landAsked = true; load(); } // a laden leg landed: that read banks the ore
      if (!ambush && op.leg === 'homeward' && op.dig && op.dig.ended_by === 'pulled' && p >= 0.5 && !midAsked && !held()) {
        midAsked = true; // an ambush on this haul reveals on the first read at or after the midpoint
        load();
      }
    }
  }
  // The markers: two whose centres are less than a radius apart (both at the gateway) sit side by side.
  const mr = Math.max(5, s * 0.45);
  const spots = gateway ? order.map(o => {
    const h = alongPath(wayPoints(o), at(o)); // BP-20b (F-C1): the marker walks the drawn way
    const [x, y] = m.hexToScreen(h.q, h.r);
    return { x, y };
  }) : [];
  for (const sp of spots) {
    const near = spots.filter(t => Math.hypot(t.x - sp.x, t.y - sp.y) < mr);
    const dx = near.length > 1 ? (near.indexOf(sp) - (near.length - 1) / 2) * 1.2 * mr : 0;
    ctx.beginPath();
    ctx.arc(sp.x + dx, sp.y, mr, 0, Math.PI * 2);
    ctx.fillStyle = COLOUR.marker;
    ctx.fill();
    ctx.lineWidth = line;
    ctx.strokeStyle = COLOUR.edge;
    ctx.stroke();
  }
  if (DEBUG) m.canvas.dataset.opsMarkers = String(spots.length);
  const so = shownOp();
  if (so) {
    const p = at(so);
    if (phaseOf(so, p) !== shownPhase || (p < 1 && !isAmbush(so) && secsLeft(so) !== shownSecs)) refresh();
  } else if (homeKey() !== null && homePhase() !== shownPhase) refresh(); // BP-18b-2b (R4)
  if (opsOut().some(o => !isAmbush(o) && progress(o) < 1) && hubShown()) m.requestFrame();
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
  if (shownOp()) { // the strip stays while the team is out; Next mission's pick takes a pin
    if (picking && !confirming) choose(pick(x, y));
    return;
  }
  selected = pick(x, y);
  if (map) map.requestFrame(); // BP-20b: the sheet's way on the map
  picker = false;
  noDigOpen = false;
  bankNote = null; crewNote = null; tripNote = null;
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
  for (const s of sites) if (Array.isArray(s.routes) && s.routes.length > 0) ways.set(`${s.q},${s.r}`, s.routes); // BP-20b
  gateway = body.gateway || null;
  const before = op;
  const was = banked;
  const wasCredits = credits;
  op = body.active_op || null;
  lastBody = body;
  // BP-18b-2a: the shown op stays while it is out, else falls back to active_op's. BP-18b-2b (R3): a home key stays
  // while its Section is still at home and Other squad can reach it.
  const home = homeKey();
  if (home !== null) {
    if (!((homeSquads().some(s => s.section_id === home) && canOther()))) shownKey = op ? opKey(op) : null;
  } else if (!opsOut().some(o => opKey(o) === shownKey)) shownKey = op ? opKey(op) : null;
  if (picker && (opsOut().length > 0 || rosterSections().length < 2)) picker = false; // R6: no picker beside an op
  banked = Number(body.ore_banked) || 0;
  credits = Number(body.credits_banked) || 0;
  base = body.base || {};
  // BP-21b: a paid building goes up over the server's seconds. Its moment plays on the body that first shows it
  // standing, while the sheet is open (Reviewer BP-10b F1): its own POST's (an instant build) or any body after one
  // that named it (Reviewer BP-21a F2: the completion read, a settle, the mission route). The first body after
  // attach has no construction before it, so a reload never plays it, and a reopen adopts nothing (b13).
  buildKept = construction;
  construction = body.construction && typeof body.construction.building === 'string' && typeof body.construction.done_at === 'string' ? body.construction : null;
  const stood = buildKept && base[buildKept.building] === true ? buildKept.building : null;
  justBuilt = baseOpen ? (built && base[built] === true ? built : stood) : null;
  justPaid = baseOpen && built && (base[built] === true || Boolean(construction && construction.building === built)) ? built : null;
  // The completion read, once per done_at. A body that still names it after the read went out (Reviewer BP-21a F5)
  // gets one re-read, timed from that body's own now; after that the next refresh carries it. Never a loop.
  if (!construction) {
    if (readTimer !== null) clearTimeout(readTimer);
    readTimer = null;
    readArmed = null;
  } else if (construction.done_at !== readArmed) {
    readArmed = construction.done_at;
    armBuildRead(Date.parse(construction.done_at) - serverNow() + BUILD_READ_SLACK_MS);
  } else if (readTimer === null && rereadFor !== construction.done_at) {
    rereadFor = construction.done_at;
    armBuildRead(Math.max(BUILD_REREAD_MS, Date.parse(construction.done_at) - Date.parse(body.now) + BUILD_READ_SLACK_MS));
  }
  notice = '';
  crewFight = body.crew_fight || null;
  const crewNow = JSON.stringify(crewFight);
  const crewLoss = crewNow !== crewKept && Boolean(crewFight) && crewFight.heads_lost > 0; // a new Crew loss (spec check F1)
  crewKept = crewNow;
  const digNow = digIn(body);
  const digLeft = digKept === true && !digNow; // BP-19c: the server cleared a dig, after settle_leaders paid its Foreman
  digKept = digNow;
  if (shownOp()) selected = null;
  else if (selected) selected = siteOf(selected.site_id); // the board may have moved on (Reviewer BP-9a F4)
  if (!canNext(shownOp())) { picking = false; pickSite = null; confirming = false; }
  else if (pickSite) pickSite = siteOf(pickSite.site_id);
  const key = op ? `${op.site_id}:${op.leg}:${op.depart_at}` : null;
  if (key !== legKey) { legKey = key; midAsked = false; landAsked = false; }
  if (op) bankNote = null;
  else if (banked > was && before && before.leg === 'homeward' && before.dig && before.dig.carried > 0) {
    bankNote = fill(DIG_COPY.banked, { gained: banked - was });
  }
  if (op) crewNote = null;
  else if (before && crewFight && crewFight.result === 'wiped') crewNote = DIG_COPY.crewWiped; // the wipe's own body: before is the op it cleared
  // BP-20b (Reviewer BP-20a-2 F1): each report shows once by (campaign, seq). A newest seq below the one shown, or the
  // same seq at another site, is a reset (a restart, an eviction, a wipe), never "seen". The two newest unseen show.
  const reports = Array.isArray(body.trip_reports) ? body.trip_reports : [];
  const top = reports.length > 0 ? reports[reports.length - 1] : null;
  const reset = Boolean(tripSeen && top && (top.seq < tripSeen.seq || (top.seq === tripSeen.seq && top.site_id !== tripSeen.site_id)));
  const shownTo = tripSeen && !reset ? tripSeen.seq : 0;
  const unseen = reports.filter(r => r.seq > shownTo).slice(-2);
  if (unseen.length > 0) { tripNote = unseen.map(tripLine); tripPending = { seq: top.seq, site_id: top.site_id }; }
  const topKey = top ? `${top.seq}:${top.site_id}` : null;
  const tripNew = topKey !== null && topKey !== tripTop; // a new report moved a leader's stress or a Crew's morale
  tripTop = topKey;
  if (!op && gateway && !noDigOffered && !sites.some(st => st.kind === 'dig')) {
    noDigOffered = true;
    noDigOpen = true;
  }
  // A fight latched on this read: a raid's or an ambush's key went away, or, per op (BP-18b-2a, R3), a
  // contract that stood outbound and unfought is now fought, or gone (a Wipeout clears its op).
  const raidLatched = Boolean(battleKey(before) && battleKey(before) !== battleKey(op));
  const prevMap = prevOps || new Map();
  const nowMap = new Map(opsOut().map(o => [opKey(o), { kind: o.kind, leg: o.leg, fought: o.fought || null }]));
  prevOps = nowMap;
  const latched = [...prevMap].filter(([k, p]) => p.kind === 'contract' && p.leg === 'outbound' && !p.fought && (!nowMap.has(k) || nowMap.get(k).fought)).map(([k]) => k);
  // One latch takes the whole bank delta (a heal, replace or hire body that latches pays for itself first, H4);
  // two in one read can't be split, so each shows no figure (the named residual, R3).
  for (const k of latched) wonBy.set(k, latched.length === 1 ? Math.max(0, credits - wasCredits + spent) : 0);
  for (const k of [...wonBy.keys()]) if (!nowMap.has(k)) wonBy.delete(k);
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
  if (raidLatched || latched.length > 0 || crewLoss) rereadRoster();
  const owed = digLeft || rosterDue || tripNew; // BP-19c: once per held → empty, per campaign entry, per hub return after an AAR; BP-20b: per new trip report
  rosterDue = false;
  if (owed && !melted && !(raidLatched || latched.length > 0 || crewLoss)) freshRoster(); // a reread above already came
  if (latched.length > 0) armRefresh();
  if (opsOut().some(relicWon) && hubShown() && !cardSeen(campaignId)) showCard({ commander: commanderName() }, () => deps.enterHub()); // A12: DEMO-001 after a relic win, on the hub only
  if (opening && !unusableLead(op)) fight(); // BP-18b-2a (R6): no fight opens that nobody can lead
}

// The stored roster is read again (a meltdown cost Crew heads; a fight cost soldiers), so the Crew
// line, a reload's reveal and the next squad (main.js's roster, through deps.setRoster) all show
// the new headcount. Every Section at 0 ends the run (BP-18b-1). Under the roster limiters, not the ops ones.
// BP-18b-2b (R10): resolves true once the new roster is kept, false on a failure or a campaign switch.
async function rereadRoster() {
  const id = campaignId;
  try {
    const body = await getRoster(id);
    if (id !== campaignId) return false;
    store.setRoster(id, body);
    deps.setRoster(body);
    const sections = Array.isArray(body.sections) ? body.sections : [];
    if (sections.length > 0 && sections.every(s => s.headcount === 0)) {
      if (hubShown()) endOfRun('soldiers');
      else endOnHub = 'soldiers'; // off the hub (an AAR may be showing): the end of run waits for it
      return true;
    }
    refresh();
    return true;
  } catch { /* the line keeps the old count; the next roster load corrects it */ }
  return false;
}

// BP-19c: a roster read after a body that ran settle_leaders (every ops route does), so the bench, a landed dig's
// stress and XP and the band words show; an open Company screen is drawn again. A failed read is owed again at the
// next adopt.
function freshRoster() {
  rereadRoster().then(ok => {
    if (ok) redrawCompany();
    else if (campaignId) rosterDue = true;
  });
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
// `built`: the building a build POST asks for, so its success gets the sheet's pop, and its moment once a body
// shows it standing (BP-21b).
// `spentOf(res)`: what a heal or replace spent, from its own body (H4). `beforeAdopt(res)`: run
// on a 200 before the body is adopted (replace patches the kept roster there, BP-12b). `afterAdopt(res)`:
// run on a 200 after it (the squad hire re-reads the roster, BP-18b-1).
async function send(path, body, built, spentOf, beforeAdopt, afterAdopt) {
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
    adopt(res, built, spentOf ? spentOf(res) : 0);
    if (afterAdopt) afterAdopt(res);
    return;
  }
  refresh();
  if (stale(err) || rated(err)) return;
  if (err instanceof ApiError && err.kind === 'conflict' && ask) {
    // BP-12b: heal's and replace's refusals by exact text; the bank comes from the re-read, never from here.
    if (err.message === REFUSAL.oreShort) { // BP-28b: a sale's 409; the bank comes from the re-read
      companyNotice = { ...ask, kind: 'ore' };
      return load();
    }
    if (err.message === REFUSAL.short || err.message === REFUSAL.out) {
      companyNotice = { ...ask, kind: err.message === REFUSAL.short ? 'short' : 'out' };
      return load();
    }
    if (err.message === REFUSAL.kitOut || err.message === REFUSAL.notOnShelf || err.message === REFUSAL.shelfFull) { // BP-23b
      companyNotice = { ...ask, kind: err.message === REFUSAL.kitOut ? 'out' : err.message === REFUSAL.notOnShelf ? 'gone' : 'shelfFull' };
      return load();
    }
    if (err.message === REFUSAL.cap) { // BP-18b-1: the squad hire's cap
      companyNotice = { ...ask, kind: 'cap' };
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
    // BP-18b-1 (F6): one squad at 0 with another alive: a line on the strip after the re-read (adopt clears it first).
    if (err.message === REFUSAL.squadEmpty || err.message === REFUSAL.escortEmpty) {
      const line = err.message === REFUSAL.squadEmpty ? COPY.squadEmpty : COPY.escortEmpty;
      return load().then(() => { notice = line; refresh(); });
    }
    if (err.message === REFUSAL.taken) { // BP-18b-2a: Next's pick on the other op's site; pick again
      pickSite = null;
      confirming = false;
      return load().then(() => { notice = COPY.taken; refresh(); });
    }
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

// `sectionId`: the Section the picker or a home sheet sends (BP-18b-2b), else null for today's body. A named start's
// 200 shows the op it started (R3): a guarded dig beside a contract becomes active_op, and contracts sort by Section.
function sendStart(sectionId) {
  if (!selected) return;
  picker = false;
  if (!sectionId) return send(opsPath(campaignId) + '/start', { site_id: selected.site_id, ...wayBody(selected) });
  const key = selected.kind === 'dig' ? 'dig' : sectionId;
  send(opsPath(campaignId) + '/start', { site_id: selected.site_id, section_id: sectionId, ...wayBody(selected) }, undefined, undefined, res => {
    if ((res && res.active_op)) shownKey = key;
  });
}

// BP-17b: the Crew goes alone. Guarded keeps the body above, so the escorted start is byte-identical.
// BP-18b-2b rework (W1, Reviewer F1): from a home sheet the Section stays home, so the home key would hold the sheet
// over the dig just started; a 200 names the dig instead. The body is unchanged.
function sendAlone() {
  if (!selected) return;
  const home = homeKey() !== null;
  send(opsPath(campaignId) + '/start', { site_id: selected.site_id, escort: false, ...wayBody(selected) }, undefined, undefined, res => {
    if ((home) && res && res.active_op) shownKey = 'dig';
  });
}

// "Go guard the dig" (§4e B2): the Section walks out and stands guard after the site's travel time.
function sendEscort() {
  send(opsPath(campaignId) + '/escort', undefined);
}

// BP-18b-1: Return names its op: the dig by its slot, a contract by its Section; a contract with no Section goes bare.
function sendReturn() {
  const o = shownOp(); // BP-18b-2a: the shown op
  const query = o && o.kind === 'dig' ? '?slot=dig' : o && o.section_id ? '?section_id=' + encodeURIComponent(o.section_id) : '';
  send(opsPath(campaignId) + '/return' + query, undefined);
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
  send(opsPath(campaignId) + '/heal', { section_id: sectionId }, false, res => healPrice(res.base).each * healingIn(res, sectionId)); // the price of the base the charge came with
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

// BP-23b: the armoury. `from` is the card that asked ('shelf' or 'section'), where a refusal's line is drawn.
function sendEquip(sectionId, item, from) {
  if (held() || !campaignId) return;
  companyNotice = null;
  companyAsk = { action: 'equip', target: from };
  send(opsPath(campaignId) + '/equip', { section_id: sectionId, item: item ? { item_name: item.item_name, quality_grade: item.quality_grade } : null }, false);
}

// The team leaves the hex it stands on (cached as the next leg's origin when the move lands).
function sendNext(site) {
  const o = shownOp(); // BP-18b-2a: the shown op
  if (!site || !o || held() || !canNext(o)) return;
  nextFrom = { q: o.at.q, r: o.at.r };
  // BP-18b-1: Next names the op it moves, as Return does.
  const which = o.kind === 'dig' ? { slot: 'dig' } : o.section_id ? { section_id: o.section_id } : {};
  send(opsPath(campaignId) + '/next', { site_id: site.site_id, ...which });
}

// BP-18b-1: the squad hire (no body). A 200 re-reads the roster (the new Section and Commander), then the
// Company screen redraws with the hired line; its two 409s become the hire block's lines (send's ask map).
// BP-18b-2b (R10, Reviewer BP-18b-1 F4): the line only once the re-read has landed, and only on the Company screen
// that asked; nothing retries a skipped one (the next roster read shows the Section; a second hire answers the cap).
function sendHire() {
  if (held() || !campaignId) return;
  companyNotice = null;
  companyAsk = { action: 'hire', target: 'company' };
  const price = Number(company.hire && company.hire.credits) || 0; // BP-18b-2a: its spend, so a contract its body latches reports its pay
  send(opsPath(campaignId) + '/hire', undefined, false, () => price, undefined, () => {
    rereadRoster().then(ok => {
      if ((ok) && (!deps.$('screen-reveal').hidden)) hiredNote = true;
      redrawCompany();
    });
  });
}

// BP-28b: sell `ore` of the banked ore (the card's Sell); allowed with a team out (BP-28a). Its spend is the sale's
// proceeds, negative (spec check F-B), so a contract its body latches reports its pay, not pay plus the sale. `onSold`
// runs once the 200's body is adopted. A 409 re-reads through send's ask block; any other 4xx is the error screen.
function sendSell(ore, onSold) {
  if (held() || !campaignId) return;
  companyNotice = null;
  companyAsk = { action: 'sell', target: 'bank' };
  send(opsPath(campaignId) + '/sell', { ore }, false, () => -(ore * SELL_ORE_CREDITS), undefined, onSold);
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
    if (shown && !hubWasShown) fightPick = null; // BP-18b-2a (F1, R4): afterAar has read it; the next fight's opener sets it
    if (shown !== hubWasShown) picker = false; // BP-18b-2b (R6): the picker never outlives the hub
    if (shown && !hubWasShown && endOnHub) { // a Section lost while away ends the run now, before any read
      const kind = endOnHub;
      endOnHub = null;
      hubWasShown = shown;
      return endOfRun(kind);
    }
    if (shown && !hubWasShown && aarSeen) { // BP-19c: back from an AAR: the fight's stress and band. With an op out, the read below owes it
      aarSeen = false;
      if (op) rosterDue = true;
      else freshRoster();
    }
    if (shown && !hubWasShown && op) load(); // a contract latches on this read (Reviewer BP-11a-2 CF1); a laden haul still reads (BP-10b F3)
    hubWasShown = shown;
  }).observe(hub, { attributes: true, attributeFilter: ['hidden'] });
  new MutationObserver(() => { if (!end.hidden) writeEndNote(); }).observe(end, { attributes: true, attributeFilter: ['hidden'] });
  const aar = deps.$('screen-aar');
  new MutationObserver(() => { if (!aar.hidden) aarSeen = true; }).observe(aar, { attributes: true, attributeFilter: ['hidden'] }); // BP-19c
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
  new MutationObserver(() => { if (briefing.hidden) clearFitTimer(); else if (opFight) dressFight(); else dressApproach(); }).observe(briefing, { attributes: true, attributeFilter: ['hidden'] });
}

// New run starts a new campaign, and ore, credits and buildings belong to the campaign: the end
// screen says what stays behind.
function writeEndNote() {
  const camp = store.campaign();
  let text = '';
  if (camp && camp.campaign_id === campaignId) {
    const built = Object.keys(base).some(k => base[k] === true) || Boolean(construction); // BP-21b: one going up counts
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
  lastBody = null;
  shownKey = null;
  picker = false;
  prevOps = null;
  wonBy = new Map();
  offset = 0;
  selected = null;
  noDigOpen = false;
  noDigOffered = false;
  busy = false;
  banked = 0;
  credits = 0;
  sentApproach = null;
  company = { sections: [], trophies: [] };
  adoptedFor = null;
  companyAsk = null;
  companyNotice = null;
  bankNote = null; crewNote = null; tripNote = null;
  notice = '';
  seenBattle = null;
  legKey = null;
  midAsked = false;
  landAsked = false;
  base = {};
  baseOpen = false;
  justBuilt = null;
  justPaid = null; // BP-21b
  construction = null;
  buildKept = null;
  readArmed = null;
  rereadFor = null;
  if (readTimer !== null) clearTimeout(readTimer);
  readTimer = null;
  if (baseTick !== null) clearTimeout(baseTick);
  baseTick = null;
  titles = new Map();
  ways = new Map(); // BP-20b
  way = 'fast';
  waySite = null;
  tripPending = null;
  tripTop = null;
  tripSeen = campaignId ? loadTripSeen(campaignId) : null;
  walk = campaignId ? loadWalk(campaignId) : null;
  nextFrom = null;
  picking = false;
  pickSite = null;
  confirming = false;
  refreshDue = 0;
  fightHead = null;
  fightPick = null;
  hiredNote = false;
  endOnHub = null;
  listOpen = false;
  crewFight = null;
  crewKept = null;
  digKept = null;
  rosterDue = true; // BP-19c (spec check F2): the boot roster was read before any settle_leaders; the first adopt rereads
  aarSeen = false;
  if (guardTimer !== null) clearTimeout(guardTimer);
  guardTimer = null;
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
  lineNodes = [el('p', 'ops-line'), el('p', 'ops-line'), el('p', 'ops-line'), el('p', 'ops-line')]; // BP-18b-2b: a fourth (a sheet's Leads line)
  outNode = el('p', 'ops-line'); // BP-18b-2a: the out line with two ops out
  outNode.hidden = true;
  actions = el('div', 'ops-actions');
  buttons = [el('button', 'btn btn-primary'), el('button', 'btn'), el('button', 'btn'), el('button', 'btn')]; // the fourth: Other squad (BP-18b-2a)
  buttons.forEach((b, i) => {
    b.type = 'button';
    b.addEventListener('click', () => { if (handlers[i]) handlers[i](); });
    actions.append(b);
  });
  buttons[3].hidden = true;
  waysRow = el('div', 'ops-ways'); // BP-20b
  waysRow.hidden = true;
  wayButtons = [el('button', 'btn'), el('button', 'btn')];
  wayButtons.forEach(b => {
    b.type = 'button';
    b.addEventListener('click', () => { way = b.dataset.route === 'safe' ? 'safe' : 'fast'; refresh(); if (map) map.requestFrame(); });
    waysRow.append(b);
  });
  strip.append(titleNode, dig.node, ...lineNodes, outNode, waysRow, actions);
  bankChip = el('div', 'ops-banked');
  bankChip.hidden = true;
  baseButton = el('button', 'ops-base', BASE_COPY.button);
  baseButton.type = 'button';
  baseButton.hidden = true;
  baseButton.addEventListener('click', () => (baseOpen ? closeBase() : openBase()));
  baseSheet = createBase(sendBuild, closeBase);
  list = createList();
  // A13, BP-25: the Demo Shop screen (demo_shop.js) from the hub at any time (no ops call). Its Buy opens
  // today's checkout as before; leaving either returns to the hub.
  const shopButton = el('button', 'ops-shop', COPY.shop);
  shopButton.type = 'button';
  shopButton.addEventListener('click', () => openShop());
  deps.$('screen-hub').append(strip, bankChip, baseButton, shopButton, baseSheet.node, list.node);
  mountShop(deps); // BP-25
  endNote = el('p', 'end-ops');
  endNote.hidden = true;
  deps.$('btn-new-run').before(endNote);
  setAarNote(aarNote);
  setSquadSize(fitToFight);
  setFightSquad(() => fightPick); // BP-18b-1 (H1): the fight sends the fought op's Commander and Section
  setDeployExtra(() => { sentApproach = pendingApproach; return pendingApproach ? { approach: pendingApproach } : {}; });
  setStill(sceneFor);
  mountStory(deps, { family: () => (opFight && fightDress ? fightDress.family : null) }); // B4 (BP-15b): the fight log
  const ms = store.missions(); if (ms && Object.values(ms).some(m => !('stance' in m))) store.setMissions(null);
  createApproaches();
  setBeforeOffer(() => deps.enterHub()); // A13: no checkout after a mission in the demo; the Demo Shop button opens it
  // BP-12b: the Company screen's view of this file's state and requests.
  mountCompany(deps, {
    adopted: () => Boolean(campaignId) && adoptedFor === campaignId && Boolean(store.campaign()) && store.campaign().campaign_id === campaignId,
    credits: () => credits,
    ore: () => banked, // BP-28b: ore_banked, from the last adopted body
    base: () => base,
    out: () => Boolean(op),
    sectionOut: id => opsOut().some(o => o.section_id && o.section_id === id), // every op (F5); not busy on an unguarded dig (section_busy)
    leaderOf: id => { const o = opsOut().find(x => x.section_id && x.section_id === id); return o ? o.commander_id || null : null; }, // BP-18b-1: "Led by" an op's Commander; every op (F5)
    freeLead, // BP-18b-2b (R9): "Led by" for a Section at home: the Commander its start would send
    fightSection,
    company: () => company,
    hired: () => hiredNote,
    clearHired: () => { hiredNote = false; },
    hire: sendHire,
    held,
    serverNow,
    bay: id => company.sections.find(s => s.section_id === id) || { wounded: 0, healing: 0, healed_at: null },
    bayOf,
    squadOf: section => fitToFight({ section }),
    trophies: () => company.trophies,
    notice: () => companyNotice,
    clearNotice: () => { companyNotice = null; },
    heal: sendHeal,
    sell: sendSell, // BP-28b
    replace: sendReplace,
    kit: id => (company.kit || []).find(k => k.section_id === id) || null, // BP-23b
    equip: sendEquip,
  });
  watch();
  mountReview(deps); // BP-27: after watch(), so its hub observer runs after the end-of-run check
  onMapCreated(attach);
}
