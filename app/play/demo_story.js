// B4's fight log and outcome write-ups (BP-15b, §4e rank 5; Wiki copy). Demo-only (ODQ-69).
// The server makes one roll; the log retells it from the mission response alone: who stood, who was hurt
// and by how much (`hp_lost`), who fell (`final_status`). It adds no hit, no damage and no death, and it
// never shows `status_note`, `unit_type` or `emoji`. Flavour is picked by a hash of `report_id`, so one
// report always reads the same; nothing here draws a random number. The log runs in battle.js's
// setBeforeOutcome step, between the roll and the banner, and never states the outcome: the banner does,
// straight after. The write-up is keyed on `report.outcome` alone (never the margin), so a Wipeout after a
// won roll reads as a Wipeout. This file owns the log's one timer, the battle screen's hide observer that
// clears it, and the hook, so no demo_ops.js frame is on a tick's stack (the harness counts those timers).

import { setBeforeOutcome } from './battle.js';

const TICK_MS = 400; // one line every 0.4 s; Skip is live throughout
const MAX_LINES = 8; // the opener, the grouped Ready line and the hurt units, the last ones paired to fit

// Every pool says the same fact several ways; the hash picks the wording, never the fact.
const STORY = {
  opener: {
    defence: [
      'Dust on the horizon, then raiders on the wire.',
      'The raiders come over the slag heap at a dead run.',
      'First shot from the ridge. Then all of them.',
    ],
    assault: [
      'In over the wall. Subtlety was not in the budget.',
      'The gate was locked. It was a short argument.',
      'The Section goes in through the front, loudly.',
    ],
    expedition: [
      'The frigate is still ticking as the Section climbs in.',
      'Somebody else wants the wreck too, and brought guns.',
      'The hold opens with a crowbar and a short prayer.',
    ],
    ambush: [
      'The raccoons come off the ridge on dirt bikes.',
      'The truck stops. The raccoons do not.',
      'Ambush on the road home. Of course it is.',
    ],
    none: [
      'Contact. Everyone remembers their training, roughly.',
      'Contact front. The paperwork can wait.',
      'The shooting starts. Nobody is surprised.',
    ],
  },
  ready: ['Not a scratch on {names}.', '{names}: unhurt, and smug about it.', 'The enemy misses {names} entirely.'],
  wounded: [
    '{name} takes a hit: {hp} HP lost. Still up.',
    '{name} is clipped for {hp} HP and says so loudly.',
    '{name} catches one, {hp} HP lost, keeps firing.',
  ],
  kia: ['{name} goes down and stays down.', '{name} does not get up.', '{name} is killed in action.'],
  woundedPair: [
    '{a} and {b} are hit: {ha} and {hb} HP lost.',
    '{a} loses {ha} HP, {b} loses {hb}. Both still up.',
    '{a} and {b} are hit, {ha} and {hb} HP. They complain in harmony.',
  ],
  kiaPair: ['{a} and {b} go down together.', '{a} and {b} do not get up.', '{a} and {b} are killed in action.'],
  mixedPair: [
    '{a} is hit for {ha} HP. {b} does not get up.',
    '{a} loses {ha} HP. {b} goes down and stays down.',
    '{a} takes {ha} HP. {b} is killed in action.',
  ],
};
// One line per outcome, true after every family and on both sides of the roll. None repeats the banner,
// mentions pay or claims nobody was hurt (the pay box and the loss lines say those).
const WRITEUP = {
  Success: 'Objective taken. The survivors are already improving the story.',
  Defeat: 'It did not go to plan. The enemy will be telling this one for years.',
  Wipeout: 'The paperwork for this one will be very short.',
};

const STORY_CSS = `
#screen-battle #battle-lines[hidden], #screen-battle .fight-log[hidden], #screen-aar .aar-writeup[hidden] { display: none; }
#screen-battle .fight-log { list-style: none; margin: 0; padding: 0; width: 100%; display: flex; flex-direction: column; gap: 0.8cqw; text-align: left; }
#screen-battle .fight-log li { padding: 0.6cqw 3cqw; font-size: 3.9cqw; line-height: 1.3; background: var(--panel); border-radius: 1.2cqw; }
#screen-battle .fight-log .log-opener { font-family: var(--display); font-weight: 700; color: var(--accent); background: none; }
#screen-battle .fight-log .log-kia { color: var(--danger); }
#screen-aar .aar-writeup { margin: 0; width: 100%; font-size: 3.8cqw; line-height: 1.35; text-align: left; }
#screen-aar .aar-verdict { color: var(--accent); }
`;

// FNV-1a over the report id, then one mix per line slot: the same report always picks the same wording.
const hash = s => { let h = 0x811c9dc5; for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 0x01000193); } return h >>> 0; };
const pick = (pool, h, slot) => pool[(Math.imul(h ^ Math.imul(slot + 1, 0x85ebca6b), 0x9e3779b1) >>> 0) % pool.length];
const fill = (text, values) => text.replace(/\{(\w+)\}/g, (m, k) => (k in values ? String(values[k]) : m));
const nameList = names => (names.length < 2 ? names.join('') : `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`);
const isHurt = u => u.hp_lost > 0; // one filter: the Ready units are its complement, so every unit is on exactly one line
const dead = u => u.final_status === 'KIA'; // the demo has no MIA path (resolver.rs)
const lost = u => u.hp_lost; // what a line shows and stamps: the AAR row says the same number

// The log's lines for one report: [{ kind, text, units, hp }]. Pure: the same report gives the same lines.
// The hurt units run Wounded first, then KIA, each in squad order; past the budget the last ones pair up
// two to a line (at most one pair in an eight-unit squad), so no unit is dropped or split.
export function fightLog(report, family) {
  const units = (report && report.unit_results) || [];
  const h = hash(String((report && report.report_id) || ''));
  let slot = 0;
  const say = (pool, values) => fill(pick(pool, h, slot++), values);
  const lines = [{ kind: 'opener', text: say(STORY.opener[family] || STORY.opener.none, {}), units: [], hp: [] }];
  const hurt = units.filter(isHurt);
  const ready = units.filter(u => !isHurt(u));
  if (ready.length) lines.push({ kind: 'ready', text: say(STORY.ready, { names: nameList(ready.map(u => u.unit_name)) }), units: ready, hp: ready.map(lost) });
  const order = [...hurt.filter(u => !dead(u)), ...hurt.filter(dead)];
  const pairs = Math.max(0, order.length - (MAX_LINES - lines.length));
  const singles = order.length - 2 * pairs;
  for (const u of order.slice(0, singles)) {
    lines.push(dead(u)
      ? { kind: 'kia', text: say(STORY.kia, { name: u.unit_name }), units: [u], hp: [lost(u)] }
      : { kind: 'wounded', text: say(STORY.wounded, { name: u.unit_name, hp: lost(u) }), units: [u], hp: [lost(u)] });
  }
  for (let i = singles; i < order.length; i += 2) {
    const a = order[i], b = order[i + 1];
    const kind = dead(a) ? 'kia' : dead(b) ? 'mixed' : 'wounded';
    const pool = kind === 'kia' ? STORY.kiaPair : kind === 'mixed' ? STORY.mixedPair : STORY.woundedPair;
    lines.push({ kind, text: say(pool, { a: a.unit_name, b: b.unit_name, ha: lost(a), hb: lost(b) }), units: [a, b], hp: [lost(a), lost(b)] });
  }
  return lines;
}

let family = () => null;
let battle = null, rows = null, log = null, writeup = null, timer = null;

// The setBeforeOutcome step: the log takes the score rows' slot and ticks in; next() reveals the banner.
function tell({ report }, next) {
  const lines = fightLog(report, family());
  rows.hidden = true;
  log.replaceChildren();
  log.hidden = false;
  let i = 0;
  const tick = () => {
    timer = null;
    if (i >= lines.length) return next();
    const l = lines[i++];
    const li = document.createElement('li');
    li.className = `log-${l.kind}`;
    li.textContent = l.text;
    if (l.units.length) {
      li.dataset.unit = l.units.map(u => u.unit_id).join(' ');
      li.dataset.hp = l.hp.join(' ');
    }
    log.append(li);
    timer = setTimeout(tick, TICK_MS);
  };
  tick();
}

// Every way off the battle screen hides it (Skip, an error screen, the end of the run): the timer dies
// here, and the score rows come back for the next fight (present() resets their children, not `hidden`).
function leave() {
  if (!battle.hidden) return;
  clearTimeout(timer);
  timer = null;
  log.replaceChildren();
  log.hidden = true;
  rows.hidden = false;
}

// The AAR's write-up (keyed on the outcome alone), then the verdict on the pick, in one node under the banner.
export function writeAar(report, verdict) {
  const text = WRITEUP[report && report.outcome] || null;
  writeup.replaceChildren();
  if (text) writeup.append(span('aar-writeup-text', text));
  if (text && verdict) writeup.append(' ');
  if (verdict) writeup.append(span('aar-verdict', verdict));
  writeup.hidden = !writeup.childNodes.length;
}

function span(className, text) {
  const node = document.createElement('span');
  node.className = className;
  node.textContent = text;
  return node;
}

export function mountStory(deps, view) {
  family = view.family;
  const style = document.createElement('style');
  style.textContent = STORY_CSS;
  document.head.append(style);
  battle = deps.$('screen-battle');
  rows = deps.$('battle-lines');
  log = document.createElement('ul');
  log.className = 'fight-log';
  log.hidden = true;
  rows.after(log);
  writeup = document.createElement('p');
  writeup.className = 'aar-writeup';
  writeup.hidden = true;
  deps.$('aar-outcome').after(writeup);
  new MutationObserver(leave).observe(battle, { attributes: true, attributeFilter: ['hidden'] });
  setBeforeOutcome(tell);
}
