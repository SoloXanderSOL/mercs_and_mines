// demo-only (BP-9b, BP-10b, BP-11b-1): the dig site's panel, the Hold-or-Fold screen (Breakpoint §4d). Never ported to dev.
//
// Pure rendering for demo_ops.js: the heat bar with its threshold tick, the status, risk and Crew
// lines, and the copy for the dig's beats. It holds no state beyond the last ore and heat figures
// it drew (for BP-37c's "+N ORE" / "+N HEAT" pop) and makes no calls. Every number comes from the server's `dig` view and
// the roster the client already holds, so the client keeps no heat or ore constant. Every string
// goes in through textContent.

// Booth copy, verbatim from the BP-9b prompt (range, risk and riskHot from BP-13's).
export const DIG_COPY = {
  drill: 'Keep drilling',
  pull: 'Pull out',
  home: 'Head home',
  fight: 'Fight',
  status: '{n} drill{s} done · {ore} ore to haul · Heat {heat}/{threshold}', // BP-47: drills done; the pile at the dig, not yet hauled home
  range: 'Next drill: +{min}–{max} heat, +{ore} ore',
  risk: '{pct}% raid risk.',
  riskHot: '{pct}% raid risk. The drill might overheat.',
  melts: 'Next drill melts the site down.',
  crew: 'Crew {heads} · Foreman {name}, {spec}',
  crewNoSpec: 'Crew {heads} · Foreman {name}',
  crewBand: 'Crew {heads} · Foreman {name}, {spec} · {band}', // BP-19c: with the roster's band word
  crewNoSpecBand: 'Crew {heads} · Foreman {name} · {band}',
  crewOnly: 'Crew {heads}',
  // BP-17b (Wiki copy): the escort and the Crew's own fights.
  unguarded: 'No guard. Raiders face the Crew alone.',
  guardWalking: 'The Section is on its way. On guard in {n} s.',
  crewHeld: 'The Crew saw the raiders off. Nobody lost.',
  crewHeldAmbush: 'The Crew beat off the ambush. Nobody lost.',
  crewRetreat: 'The Crew fought the raiders and lost miners.', // BP-34: no 'alone' (a sent guard may be on its way)
  crewRetreatAmbush: 'The Crew was ambushed on the road and lost miners.',
  crewWiped: 'The Crew was overrun. Nobody came back, and neither did the ore.',
  // BP-34 (Wiki copy, draft for the script pass): the Crew-fight card on the hub.
  cardRaid: 'Raid at the dig',
  cardAmbush: 'Ambush on the road home',
  cardRaidHeld: 'Raccoons on dirt bikes hit the dig. The miners grabbed their picks and saw them off.',
  cardRaidRetreat: 'Raccoons on dirt bikes hit the dig. The miners fought, then fell back with what they could carry.',
  cardRaidWiped: 'Raccoons on dirt bikes overran the dig. Nobody came back, and neither did the ore.',
  cardAmbushHeld: 'Raccoons jumped the haul halfway home. The miners held on to the truck.',
  cardAmbushRetreat: 'Raccoons jumped the haul halfway home. The miners broke and ran for the gateway.',
  cardAmbushWiped: 'Raccoons jumped the haul halfway home. Nobody came back, and neither did the ore.',
  cardLostNone: 'Lost: nobody.',
  cardLost: 'Lost: {n} of the Crew.',
  cardGuardLate: 'The guard was still {n} s away.',
  cardOk: 'Carry on',
  pop: '+{n} ORE',
  popHeat: '+{n} HEAT', // BP-37c (item 6): the heat that drill added, never the total
  raidWait: 'Raiders at {title}. Fight to resume drilling.',
  ambushWait: 'Ambushed on the road. Fight to bring the haul home.',
  raidLead: 'Claim jumpers at {title}: raccoons on dirt bikes, they seem very interested in your drill. Drilling halts until the site is secured. They cannot read the Keep Out sign. Explain it to them - forcefully.',
  ambushLead: 'Your haul has been intercepted on the road home. {carried} ore is at stake, and the raccoons on the ridge have already started arguing about how they\'ll divide it up. Get it back on the truck.',
  incident: 'INCIDENT REPORT',
  // BP-49b (Director-approved copy, 2026-11-13): the held meltdown's report, then its leg home after Return to base.
  melt: 'MELTDOWN!',
  meltdown: 'The crew kept drilling at {title} long after the gauge said stop. The bit melted into the vein and took the dig with it.',
  writtenOff: 'Ore on site: written off.',
  lost: '{n} miners lost. Replacements can be hired at base.',
  lostOne: '1 miner lost. Replacements can be hired at base.',
  cause: 'Cause of incident, per the Foreman: "The gauge was taped over."',
  back: 'Return to base',
  meltdownHome: 'Heading home. {n} s',
  walkedAway: 'Site abandoned. The ore stays in the ground. {n} s',
  retreat: 'Fell back with {carried} ore. The rest is a write-off. {n} s',
  pulled: 'Hauling {carried} ore home. {n} s',
  banked: '+{gained} ore banked.',
  bankedLine: 'Banked: {ore} ore · {credits} credits',
  rate: 'Command is rationing requests. Try again in {n} s.',
  endBanked: 'Banked ore and credits do not transfer. They stay on the old books.',
  endOut: 'Banked ore and credits do not transfer. Ore on site and in transit is written off.',
};

// The bar runs from 0 to 1.2 × threshold, so the threshold tick sits at 1 / 1.2 of its width.
const SCALE = 1.2;
const HOT = 0.7; // the fill turns from orange to red at this share of the threshold
const TREMBLE = 0.5; // BP-37c (item 7): the heat number trembles from this share of the threshold
const DIG_ART = 'scene_dig_before.jpg'; // BP-49a: the dig's scene while the Crew is at the site
const DIG_ART_AFTER = 'scene_dig_after.jpg'; // BP-49b: the held meltdown's report

export const DIG_CSS = `
.dig-panel { position: relative; display: flex; flex-direction: column; gap: 1.2cqw; }
.dig-panel[hidden], .dig-line[hidden], .ops-banked[hidden], .end-ops[hidden] { display: none; }
.dig-art { display: block; width: 100%; height: auto; aspect-ratio: 1050 / 586; border-radius: 1cqw; } /* BP-49a: the dig's scene, above the vein title */
.dig-art[hidden] { display: none; }
.dig-melt { position: absolute; left: 0; right: 0; bottom: calc(100% + 12.8cqw); margin: 0; text-align: center; font-family: var(--display); font-weight: 800; font-size: 7cqw; line-height: 1.1; color: var(--danger); text-shadow: 0 0 1.5cqw #000; pointer-events: none; } /* BP-49b: MELTDOWN!, where the pops were */
.dig-melt[hidden], .dig-report-head[hidden] { display: none; }
.dig-report-head { margin: 0; font-family: var(--display); font-weight: 800; font-size: 4.6cqw; line-height: 1.2; letter-spacing: 0.04em; color: var(--danger); } /* BP-49b: INCIDENT REPORT */
.dig-heat { position: relative; height: 4cqw; background: var(--panel); border: 0.3cqw solid var(--panel-edge); border-radius: 1cqw; overflow: hidden; }
.dig-heat-fill { position: absolute; left: 0; top: 0; bottom: 0; width: 0; background: #e0682c; transition: width 0.25s ease-out, background-color 0.25s ease-out; } /* BP-47: orange, never the ore's accent yellow */
.dig-heat-fill.hot { background: var(--danger); }
.dig-heat-tick { position: absolute; top: 0; bottom: 0; left: ${(100 / SCALE).toFixed(3)}%; width: 0.6cqw; margin-left: -0.3cqw; background: #fff; }
.dig-heat-ghost, .dig-heat-over { position: absolute; top: 0; bottom: 0; left: 0; width: 0; pointer-events: none; }
.dig-heat-ghost { background: #e0682c; opacity: 0.3; } /* BP-47: the fill's orange */
.dig-heat.drilling { animation: dig-tremble 0.125s linear infinite; } /* BP-47 (item 4): the tap's shake, until the answer */
.ops-actions .btn.drill-press, .ops-actions .btn.drill-press:disabled { transform: scale(0.94); opacity: 1; } /* BP-47 (item 4): pressed in, not greyed, while the drill is out */
.dig-heat-over { background: var(--danger); opacity: 0.55; }
.dig-heat-over[hidden] { display: none; }
.dig-line { margin: 0; font-size: 3.9cqw; line-height: 1.35; }
.dig-line.alert { color: var(--accent); font-weight: 700; }
.dig-line.danger { color: var(--danger); font-weight: 700; }
.dig-pops { position: absolute; left: 0; right: 0; bottom: calc(100% + 12.8cqw); display: flex; justify-content: center; align-items: baseline; gap: 6cqw; font-family: var(--display); font-weight: 800; font-size: 7cqw; line-height: 1.1; text-shadow: 0 0 1.5cqw #000; pointer-events: none; } /* BP-37c */
.dig-pop { color: var(--accent); opacity: 0; }
.dig-pop-heat { color: var(--danger); opacity: 0; }
.dig-shake { display: inline-block; }
.dig-pop.shown, .dig-pop-heat.shown { animation: dig-pop 2s linear; }
@keyframes dig-pop { 0%, 75% { opacity: 1; transform: none; } 100% { opacity: 0; transform: translateY(-3cqw); } }
@keyframes dig-hold { from, to { opacity: 1; } }
.dig-pop-heat.shown.tremble .dig-shake { animation: dig-tremble 0.125s linear 16; }
.dig-pop-heat.shown.rattle .dig-shake { animation: dig-rattle 0.08s linear 25; }
@keyframes dig-tremble { 0%, 100% { transform: none; } 25% { transform: translate(-0.4cqw, 0.2cqw); } 75% { transform: translate(0.4cqw, -0.2cqw); } }
@keyframes dig-rattle { 0%, 100% { transform: none; } 25% { transform: translate(-1cqw, 0.5cqw); } 75% { transform: translate(1cqw, -0.5cqw); } }
.ops-strip.flash { animation: dig-flash 0.4s ease-out; }
@keyframes dig-flash { from { background: rgba(200, 68, 58, 0.9); } to { background: rgba(11, 11, 12, 0.94); } }
.ops-title.danger { color: var(--danger); }
.ops-banked { position: absolute; top: 11.5cqw; right: 3cqw; padding: 1cqw 2.5cqw; font-family: var(--mono); font-size: 3.6cqw; color: var(--accent); background: rgba(11, 11, 12, 0.94); border: 0.3cqw solid var(--panel-edge); border-radius: 1.5cqw; }
.end-ops { margin: 0; max-width: 84cqw; font-size: 4.2cqw; line-height: 1.4; color: var(--accent); }
.crew-card { position: absolute; left: 6cqw; right: 6cqw; top: 30cqw; z-index: 5; padding: 4cqw; display: flex; flex-direction: column; gap: 2.5cqw; text-align: left; background: rgba(11, 11, 12, 0.97); border: 0.5cqw solid var(--danger); border-radius: 2cqw; } /* BP-34 */
.crew-card[hidden], .crew-card-line[hidden] { display: none; }
.crew-card-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 5.5cqw; line-height: 1.15; color: var(--danger); }
.crew-card-line { margin: 0; font-size: 3.9cqw; line-height: 1.4; }
.crew-card .btn { width: 100%; max-width: none; min-height: 11cqw; margin-top: 1cqw; font-size: 4.4cqw; }
@media (prefers-reduced-motion: reduce) {
  .dig-heat-fill { transition: none; }
  .ops-strip.flash { animation: none; }
  .dig-pop.shown, .dig-pop-heat.shown { animation: dig-hold 2s steps(1, end); }
  .dig-pop-heat.shown.tremble .dig-shake, .dig-pop-heat.shown.rattle .dig-shake { animation: none; }
  .dig-heat.drilling { animation: none; }
}
`;

export const fill = (text, values) => text.replace(/\{(\w+)\}/g, (m, k) => (k in values ? String(values[k]) : m));
// BP-19c: the roster's stress band (routes/roster.rs `band`, demo_ops.rs stress_band) as its word. The client words a
// band and never computes one (Reviewer BP-19a). A missing or unknown band (a 0.3.2 server sends none) is '' and shows nothing.
export const BAND_WORDS = { rested: 'Rested', strained: 'Strained', breaking_point: 'Breaking Point', shattered: 'Shattered' };
export const bandWord = b => (typeof b === 'string' && Object.prototype.hasOwnProperty.call(BAND_WORDS, b) ? BAND_WORDS[b] : '');

// A9 (BP-11b-1): the one banked figure, on the chip and the Base sheet alike.
export const bankedLine = (ore, credits) => fill(DIG_COPY.bankedLine, { ore, credits });

function el(tag, className) {
  const node = document.createElement(tag);
  node.className = className;
  return node;
}

// Restarts a one-shot CSS animation on `node`.
function replay(node, className) {
  node.classList.remove(className);
  void node.offsetWidth;
  node.classList.add(className);
}

export function createDigPanel() {
  const node = el('div', 'dig-panel');
  const heat = el('div', 'dig-heat');
  const bar = el('div', 'dig-heat-fill');
  const ghost = el('div', 'dig-heat-ghost'); // BP-13: the heat the next drill may add
  const over = el('div', 'dig-heat-over');   // the part of it past the threshold
  over.hidden = true;
  heat.append(bar, ghost, over, el('div', 'dig-heat-tick'));
  const status = el('p', 'dig-line dig-status');
  const range = el('p', 'dig-line dig-range');
  const risk = el('p', 'dig-line dig-risk');
  const crew = el('p', 'dig-line dig-crew');
  const guard = el('p', 'dig-line dig-guard');           // BP-17b: who guards the dig
  const crewFight = el('p', 'dig-line dig-crew-fight');  // BP-17b: the Crew's own fight
  const melt = el('p', 'dig-melt');                       // BP-49b: MELTDOWN!, where the pops were
  melt.textContent = DIG_COPY.melt;
  melt.hidden = true;
  const reportHead = el('p', 'dig-report-head');          // BP-49b: INCIDENT REPORT
  reportHead.textContent = DIG_COPY.incident;
  reportHead.hidden = true;
  const report = [0, 1, 2, 3].map(() => { const n = el('p', 'dig-line dig-report'); n.hidden = true; return n; }); // BP-49b: its four lines
  const pops = el('div', 'dig-pops');      // BP-37c: "+N ORE" and "+N HEAT", one line above the strip
  const pop = el('span', 'dig-pop');
  const heatPop = el('span', 'dig-pop-heat');
  const shake = el('span', 'dig-shake');   // the shake rides an inner span, so it never fights the beat's transform
  heatPop.append(shake);
  pops.append(pop, heatPop);
  for (const n of [pop, heatPop]) n.addEventListener('animationend', e => { if (e.target === n) n.classList.remove('shown'); }); // BP-37c F1: a finished beat leaves no class to replay
  node.append(heat, status, range, risk, crew, guard, crewFight, reportHead, ...report, pops, melt);
  node.hidden = true;
  const art = el('img', 'dig-art'); // BP-49a: demo_ops.js puts it at the top of the strip; it shows and hides with the panel
  art.alt = '';
  art.hidden = true;
  const p = { node, art, artBad: false, heat, bar, ghost, over, status, range, risk, crew, guard, crewFight, melt, reportHead, report, pop, heatPop, shake, lastOre: null, lastHeat: null };
  art.addEventListener('error', () => { p.artBad = true; art.hidden = true; }); // a failed picture leaves the strip whole
  return p;
}

// The Crew and Foreman the dig start picked: the roster's first of each (same listers, same order).
function crewLine(roster) {
  const crew = roster && Array.isArray(roster.crews) ? roster.crews[0] : null;
  if (!crew) return '';
  const fm = Array.isArray(roster.foremen) ? roster.foremen[0] : null;
  if (!fm) return fill(DIG_COPY.crewOnly, { heads: crew.headcount });
  const band = bandWord(fm.band); // BP-19c
  const key = fm.specialization ? (band ? 'crewBand' : 'crew') : (band ? 'crewNoSpecBand' : 'crewNoSpec');
  return fill(DIG_COPY[key], { heads: crew.headcount, name: fm.name, spec: fm.specialization, band });
}

// BP-17b: the line for the body's crew_fight (absent = none). It is sticky until the next start and has
// no id (Reviewer BP-17a F3), so demo_ops.js draws it as standing state, never as a diffed toast.
export function crewFightLine(f) {
  if (!f) return '';
  if (f.result === 'held') return f.kind === 'ambush' ? DIG_COPY.crewHeldAmbush : DIG_COPY.crewHeld;
  if (f.result === 'retreat') return f.kind === 'ambush' ? DIG_COPY.crewRetreatAmbush : DIG_COPY.crewRetreat;
  return f.result === 'wiped' ? DIG_COPY.crewWiped : '';
}

// BP-34 (item 17): the Crew-fight card from the body's crew_fight {kind, result, heads_lost}. `lateSecs` > 0: a guard
// was sent and still that many seconds out when it hit (0 = none on the way). Pure: demo_ops.js shows it.
export function crewCard(f, lateSecs) {
  const raid = f.kind === 'raid';
  const said = { held: raid ? DIG_COPY.cardRaidHeld : DIG_COPY.cardAmbushHeld, retreat: raid ? DIG_COPY.cardRaidRetreat : DIG_COPY.cardAmbushRetreat, wiped: raid ? DIG_COPY.cardRaidWiped : DIG_COPY.cardAmbushWiped }[f.result] || '';
  const lost = f.heads_lost > 0 ? fill(DIG_COPY.cardLost, { n: f.heads_lost }) : DIG_COPY.cardLostNone;
  return { title: raid ? DIG_COPY.cardRaid : DIG_COPY.cardAmbush, lines: [said, lost, lateSecs > 0 ? fill(DIG_COPY.cardGuardLate, { n: lateSecs }) : ''] };
}

// `dig`: the server's view at an arrived dig; `roster`: the stored roster body, or null;
// `extra`: BP-17b's guard and Crew fight lines from demo_ops.js ('' = hidden).
export function renderDig(p, dig, roster, extra = {}) {
  const scale = dig.threshold * SCALE;
  const share = scale > 0 ? Math.min(1, dig.heat / scale) : 0;
  p.bar.style.width = (share * 100).toFixed(2) + '%';
  p.bar.classList.toggle('hot', dig.heat >= dig.threshold * HOT);
  p.status.textContent = fill(DIG_COPY.status, { n: dig.drills, s: dig.drills === 1 ? '' : 's', ore: dig.ore_on_site, heat: dig.heat, threshold: dig.threshold });
  // BP-13: the next drill's range from the body (absent on an older server: no range, no ghost).
  const live = Number.isFinite(dig.next_heat_max) && dig.next_heat_max > 0 && !dig.raid;
  const hot = live && dig.might_overheat === true;
  // BP-37c (item 7): rattle on `hot` (live only, so a raid's or a finished dig's stale flag never rattles),
  // else a tremble from TREMBLE x threshold, else none.
  const band = hot ? 'rattle' : dig.heat >= dig.threshold * TREMBLE ? 'tremble' : '';
  const at = v => (scale > 0 ? Math.max(0, Math.min(1, v / scale)) : 0);
  const pc = v => (v * 100).toFixed(2) + '%';
  const top = live ? dig.heat + dig.next_heat_max : dig.heat;
  const from = Math.max(dig.heat, dig.threshold);
  p.ghost.style.left = pc(at(dig.heat));
  p.ghost.style.width = pc(at(top) - at(dig.heat));
  p.over.style.left = pc(at(from));
  p.over.style.width = pc(Math.max(0, at(top) - at(from)));
  p.over.hidden = !hot;
  p.range.textContent = live ? fill(DIG_COPY.range, { min: dig.next_heat_min, max: dig.next_heat_max, ore: dig.next_ore }) : '';
  p.range.hidden = !p.range.textContent;
  let risk = '', tone = '';
  if (!dig.raid) {
    // next_raid_pct is null on an arrived outbound leg with no raid only when even the bottom of the
    // next drill's range reaches the threshold; might_overheat is the top's warning (BP-13).
    if (dig.next_raid_pct === null) { risk = DIG_COPY.melts; tone = 'danger'; }
    else if (hot) { risk = fill(DIG_COPY.riskHot, { pct: dig.next_raid_pct }); tone = 'danger'; }
    else { risk = fill(DIG_COPY.risk, { pct: dig.next_raid_pct }); if (dig.next_raid_pct >= 20) tone = 'alert'; }
  }
  p.risk.textContent = risk;
  p.risk.hidden = !risk;
  p.risk.classList.toggle('alert', tone === 'alert');
  p.risk.classList.toggle('danger', tone === 'danger');
  const crew = crewLine(roster);
  p.crew.textContent = crew;
  p.crew.hidden = !crew;
  p.guard.textContent = extra.guard || '';
  p.guard.hidden = !p.guard.textContent;
  p.crewFight.textContent = extra.crewFight || '';
  p.crewFight.hidden = !p.crewFight.textContent;
  if (p.lastOre !== null && dig.ore_on_site > p.lastOre) {
    p.pop.textContent = fill(DIG_COPY.pop, { n: dig.ore_on_site - p.lastOre });
    replay(p.pop, 'shown');
  }
  p.lastOre = dig.ore_on_site;
  if (p.lastHeat !== null && dig.heat > p.lastHeat) {
    p.shake.textContent = fill(DIG_COPY.popHeat, { n: dig.heat - p.lastHeat });
    p.heatPop.classList.toggle('tremble', band === 'tremble');
    p.heatPop.classList.toggle('rattle', band === 'rattle');
    replay(p.heatPop, 'shown');
  }
  p.lastHeat = dig.heat;
  p.melt.hidden = true; // BP-49b: the report's nodes stay down on a live dig
  p.reportHead.hidden = true;
  for (const n of p.report) n.hidden = true;
  setArt(p, DIG_ART); // set at the first dig shown, so boot loads no new bytes
  p.art.hidden = p.artBad;
  p.node.hidden = false;
}

// BP-49b (Reviewer BP-49a CF2, F4): the scene is chosen on every show, never only at the first, so the report's "after"
// and the next dig's "before" each take their turn. A new src clears a failure: one dropped fetch costs that picture, not
// the session.
function setArt(p, name) {
  const src = `assets/art/${name}`;
  if (p.art.getAttribute('src') === src) return;
  p.artBad = false;
  p.art.src = src;
}

// BP-49b: the held meltdown's incident report, in the live panel's place: the "after" scene, MELTDOWN! where the pops
// were, the bar full and red, the heading and the four lines. The count is the server's meltdown_heads (the actual loss);
// an absent count (an older server) drops that line rather than guess. demo_ops.js sets the strip's title and its button.
export function renderReport(p, dig, title) {
  quietDig(p);
  p.bar.style.width = '100%';
  p.bar.classList.add('hot');
  p.ghost.style.width = '0%';
  p.over.hidden = true;
  for (const n of [p.status, p.range, p.risk, p.crew, p.guard, p.crewFight]) { n.textContent = ''; n.hidden = true; }
  const n = dig.meltdown_heads;
  const lost = Number.isInteger(n) ? fill(n === 1 ? DIG_COPY.lostOne : DIG_COPY.lost, { n }) : '';
  const lines = [fill(DIG_COPY.meltdown, { title }), DIG_COPY.writtenOff, lost, DIG_COPY.cause];
  p.report.forEach((node, i) => { node.textContent = lines[i]; node.hidden = !lines[i]; });
  p.reportHead.hidden = false;
  p.melt.hidden = false;
  p.lastOre = dig.ore_on_site;
  p.lastHeat = dig.heat;
  setArt(p, DIG_ART_AFTER);
  p.art.hidden = p.artBad;
  p.node.hidden = false;
}

// BP-37a-2 (Reviewer BP-37c rework F1, RULE-114): clears the dig's beat, so nothing replays when the panel or the hub shows again.
export function quietDig(p) {
  for (const n of [p.pop, p.heatPop]) n.classList.remove('shown');
  p.heat.classList.remove('drilling'); // BP-47 (item 4)
}

export function hideDig(p) {
  p.art.hidden = true; // BP-49a
  if (p.artBad) { p.artBad = false; p.art.removeAttribute('src'); } // BP-49b (Reviewer BP-49a F4): the next show retries
  p.node.hidden = true;
  p.lastOre = null;
  p.lastHeat = null;
  quietDig(p); // BP-37c F1 (BP-21b F1's rule): what hides the panel clears its beat
}

// The strip on a dig's homeward leg, by how the dig ended; null on a contract's leg.
export function homewardLines(dig, title, n) {
  switch (dig ? dig.ended_by : null) {
    case 'meltdown': return { title: '', danger: false, lines: [fill(DIG_COPY.meltdownHome, { n })] }; // BP-49b (F2): the report was read at the site
    case 'walked_away': return { title: '', danger: false, lines: [fill(DIG_COPY.walkedAway, { n })] };
    case 'retreat': return { title: '', danger: false, lines: [fill(DIG_COPY.retreat, { carried: dig.carried, n })] };
    case 'pulled': return { title: '', danger: false, lines: [fill(DIG_COPY.pulled, { carried: dig.carried, n })] };
    default: return null;
  }
}

// The meltdown's beat: a red flash across the strip. A finished flash clears its class (BP-37a-2, RULE-114), so
// showing the strip again never replays it. One listener per strip; the dig pops' own animationend bubbles here too.
export function flash(strip) {
  if (!strip.flashEnd) {
    strip.flashEnd = e => { if (e.target === strip && e.animationName === 'dig-flash') strip.classList.remove('flash'); };
    strip.addEventListener('animationend', strip.flashEnd);
  }
  replay(strip, 'flash');
}
