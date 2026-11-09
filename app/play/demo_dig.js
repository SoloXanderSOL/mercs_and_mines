// demo-only (BP-9b, BP-10b, BP-11b-1): the dig site's panel, the Hold-or-Fold screen (Breakpoint §4d). Never ported to dev.
//
// Pure rendering for demo_ops.js: the heat bar with its threshold tick, the status, risk and Crew
// lines, and the copy for the dig's beats. It holds no state beyond the last ore figure it drew
// (for the "+n ore" pop) and makes no calls. Every number comes from the server's `dig` view and
// the roster the client already holds, so the client keeps no heat or ore constant. Every string
// goes in through textContent.

// Booth copy, verbatim from the BP-9b prompt (range, risk and riskHot from BP-13's).
export const DIG_COPY = {
  drill: 'Keep drilling',
  pull: 'Pull out',
  home: 'Head home',
  fight: 'Fight',
  status: 'Drills {n} · {ore} ore on site · Heat {heat}/{threshold}',
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
  pop: '+{n} ore',
  raidWait: 'Raiders at {title}. Fight to resume drilling.',
  ambushWait: 'Ambushed on the road. Fight to bring the haul home.',
  raidLead: 'Claim jumpers at {title}: raccoons on dirt bikes, very interested in your drill. Drilling halts until the site is secured. They cannot read the Keep Out sign. Explain it to them.',
  ambushLead: 'Your haul has been intercepted on the road home. {carried} ore is at stake, and the raccoons on the ridge have already started dividing it up. Get it back on the truck.',
  incident: 'INCIDENT REPORT',
  meltdown: 'Thermal breach at {title}. Ore on site written off.',
  meltdownHome: 'Casualties logged against your Crew. Heading home. {n} s',
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
const HOT = 0.7; // the fill turns from amber to red at this share of the threshold

export const DIG_CSS = `
.dig-panel { position: relative; display: flex; flex-direction: column; gap: 1.2cqw; }
.dig-panel[hidden], .dig-line[hidden], .ops-banked[hidden], .end-ops[hidden] { display: none; }
.dig-heat { position: relative; height: 4cqw; background: var(--panel); border: 0.3cqw solid var(--panel-edge); border-radius: 1cqw; overflow: hidden; }
.dig-heat-fill { position: absolute; left: 0; top: 0; bottom: 0; width: 0; background: var(--accent); transition: width 0.25s ease-out, background-color 0.25s ease-out; }
.dig-heat-fill.hot { background: var(--danger); }
.dig-heat-tick { position: absolute; top: 0; bottom: 0; left: ${(100 / SCALE).toFixed(3)}%; width: 0.6cqw; margin-left: -0.3cqw; background: #fff; }
.dig-heat-ghost, .dig-heat-over { position: absolute; top: 0; bottom: 0; left: 0; width: 0; pointer-events: none; }
.dig-heat-ghost { background: var(--accent); opacity: 0.3; }
.dig-heat-over { background: var(--danger); opacity: 0.55; }
.dig-heat-over[hidden] { display: none; }
.dig-line { margin: 0; font-size: 3.9cqw; line-height: 1.35; }
.dig-line.alert { color: var(--accent); font-weight: 700; }
.dig-line.danger { color: var(--danger); font-weight: 700; }
.dig-pop { position: absolute; right: 0; top: 5cqw; font-family: var(--display); font-weight: 800; font-size: 4.6cqw; color: var(--accent); opacity: 0; pointer-events: none; }
.dig-pop.shown { animation: dig-pop 0.9s ease-out; }
@keyframes dig-pop { from { opacity: 1; transform: none; } to { opacity: 0; transform: translateY(-3cqw); } }
.ops-strip.flash { animation: dig-flash 0.4s ease-out; }
@keyframes dig-flash { from { background: rgba(200, 68, 58, 0.9); } to { background: rgba(11, 11, 12, 0.94); } }
.ops-title.danger { color: var(--danger); }
.ops-banked { position: absolute; top: 11.5cqw; right: 3cqw; padding: 1cqw 2.5cqw; font-family: var(--mono); font-size: 3.6cqw; color: var(--accent); background: rgba(11, 11, 12, 0.94); border: 0.3cqw solid var(--panel-edge); border-radius: 1.5cqw; }
.screen-hub:has(.ops-strip:not([hidden])) .legend { display: none; }
/* Off site, an open legend covers the chip and the Base button; on site the legend is hidden (Reviewer BP-9b F4). */
.screen-hub:has(.ops-strip[hidden]):has(.legend:not([hidden])) :is(.ops-banked, .ops-base) { display: none; }
.end-ops { margin: 0; max-width: 84cqw; font-size: 4.2cqw; line-height: 1.4; color: var(--accent); }
.crew-card { position: absolute; left: 6cqw; right: 6cqw; top: 30cqw; z-index: 5; padding: 4cqw; display: flex; flex-direction: column; gap: 2.5cqw; text-align: left; background: rgba(11, 11, 12, 0.97); border: 0.5cqw solid var(--danger); border-radius: 2cqw; } /* BP-34 */
.crew-card[hidden], .crew-card-line[hidden] { display: none; }
.crew-card-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 5.5cqw; line-height: 1.15; color: var(--danger); }
.crew-card-line { margin: 0; font-size: 3.9cqw; line-height: 1.4; }
.crew-card .btn { width: 100%; max-width: none; min-height: 11cqw; margin-top: 1cqw; font-size: 4.4cqw; }
@media (prefers-reduced-motion: reduce) {
  .dig-heat-fill { transition: none; }
  .dig-pop.shown, .ops-strip.flash { animation: none; }
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
  const pop = el('span', 'dig-pop');
  node.append(heat, status, range, risk, crew, guard, crewFight, pop);
  node.hidden = true;
  return { node, bar, ghost, over, status, range, risk, crew, guard, crewFight, pop, lastOre: null };
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
  p.status.textContent = fill(DIG_COPY.status, { n: dig.drills, ore: dig.ore_on_site, heat: dig.heat, threshold: dig.threshold });
  // BP-13: the next drill's range from the body (absent on an older server: no range, no ghost).
  const live = Number.isFinite(dig.next_heat_max) && dig.next_heat_max > 0 && !dig.raid;
  const hot = live && dig.might_overheat === true;
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
  p.node.hidden = false;
}

export function hideDig(p) {
  p.node.hidden = true;
  p.lastOre = null;
}

// The strip on a dig's homeward leg, by how the dig ended; null on a contract's leg.
export function homewardLines(dig, title, n) {
  switch (dig ? dig.ended_by : null) {
    case 'meltdown': return { title: DIG_COPY.incident, danger: true, lines: [fill(DIG_COPY.meltdown, { title }), fill(DIG_COPY.meltdownHome, { n })] };
    case 'walked_away': return { title: '', danger: false, lines: [fill(DIG_COPY.walkedAway, { n })] };
    case 'retreat': return { title: '', danger: false, lines: [fill(DIG_COPY.retreat, { carried: dig.carried, n })] };
    case 'pulled': return { title: '', danger: false, lines: [fill(DIG_COPY.pulled, { carried: dig.carried, n })] };
    default: return null;
  }
}

// The meltdown's beat: a red flash across the strip.
export function flash(strip) {
  replay(strip, 'flash');
}
