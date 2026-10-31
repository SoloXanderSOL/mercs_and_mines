// demo-only (BP-9b, BP-10b): the dig site's panel, the Hold-or-Fold screen (Breakpoint §4d). Never ported to dev.
//
// Pure rendering for demo_ops.js: the heat bar with its threshold tick, the status, risk and Crew
// lines, and the copy for the dig's beats. It holds no state beyond the last ore figure it drew
// (for the "+n ore" pop) and makes no calls. Every number comes from the server's `dig` view and
// the roster the client already holds, so the client keeps no heat or ore constant. Every string
// goes in through textContent.

// Booth copy, verbatim from the BP-9b prompt.
export const DIG_COPY = {
  drill: 'Keep drilling',
  pull: 'Pull out',
  home: 'Head home',
  fight: 'Fight',
  status: 'Drills {n} · {ore} ore on site · Heat {heat}/{threshold}',
  risk: 'Next drill: +{ore} ore, {pct}% raid risk',
  melts: 'Next drill melts the site down.',
  crew: 'Crew {heads} · Foreman {name}, {spec}',
  crewNoSpec: 'Crew {heads} · Foreman {name}',
  crewOnly: 'Crew {heads}',
  pop: '+{n} ore',
  raidWait: 'Raiders at {title}. Fight to resume drilling.',
  ambushWait: 'Ambushed on the road. Fight to bring the haul home.',
  raidLead: 'Claim jumpers at {title}. Drilling halts until the site is secured.',
  ambushLead: 'Your haul has been intercepted on the road home. {carried} ore is at stake.',
  incident: 'INCIDENT REPORT',
  meltdown: 'Thermal breach at {title}. Ore on site written off.',
  meltdownHome: 'Casualties logged against your Crew. Heading home. {n} s',
  walkedAway: 'Site abandoned. The ore stays in the ground. {n} s',
  retreat: 'Fell back with {carried} ore. The rest is a write-off. {n} s',
  pulled: 'Hauling {carried} ore home. {n} s',
  banked: '{gained} ore banked. Company holdings: {total}.',
  bankedChip: 'Banked: {total} ore',
  rate: 'Command is rationing requests. Try again in {n} s.',
  endBanked: 'Holdings do not transfer. {total} ore stays on the old books.',
  endOut: 'Holdings do not transfer. Ore on site and in transit is written off.',
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
@media (prefers-reduced-motion: reduce) {
  .dig-heat-fill { transition: none; }
  .dig-pop.shown, .ops-strip.flash { animation: none; }
}
`;

export const fill = (text, values) => text.replace(/\{(\w+)\}/g, (m, k) => (k in values ? String(values[k]) : m));

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
  heat.append(bar, el('div', 'dig-heat-tick'));
  const status = el('p', 'dig-line');
  const risk = el('p', 'dig-line');
  const crew = el('p', 'dig-line');
  const pop = el('span', 'dig-pop');
  node.append(heat, status, risk, crew, pop);
  node.hidden = true;
  return { node, bar, status, risk, crew, pop, lastOre: null };
}

// The Crew and Foreman the dig start picked: the roster's first of each (same listers, same order).
function crewLine(roster) {
  const crew = roster && Array.isArray(roster.crews) ? roster.crews[0] : null;
  if (!crew) return '';
  const fm = Array.isArray(roster.foremen) ? roster.foremen[0] : null;
  if (!fm) return fill(DIG_COPY.crewOnly, { heads: crew.headcount });
  return fill(fm.specialization ? DIG_COPY.crew : DIG_COPY.crewNoSpec, { heads: crew.headcount, name: fm.name, spec: fm.specialization });
}

// `dig`: the server's view at an arrived dig; `roster`: the stored roster body, or null.
export function renderDig(p, dig, roster) {
  const scale = dig.threshold * SCALE;
  const share = scale > 0 ? Math.min(1, dig.heat / scale) : 0;
  p.bar.style.width = (share * 100).toFixed(2) + '%';
  p.bar.classList.toggle('hot', dig.heat >= dig.threshold * HOT);
  p.status.textContent = fill(DIG_COPY.status, { n: dig.drills, ore: dig.ore_on_site, heat: dig.heat, threshold: dig.threshold });
  let risk = '', tone = '';
  if (!dig.raid) {
    // next_raid_pct is null on an arrived outbound leg with no raid only when the next drill melts.
    if (dig.next_raid_pct === null) { risk = DIG_COPY.melts; tone = 'danger'; }
    else { risk = fill(DIG_COPY.risk, { ore: dig.next_ore, pct: dig.next_raid_pct }); if (dig.next_raid_pct >= 20) tone = 'alert'; }
  }
  p.risk.textContent = risk;
  p.risk.hidden = !risk;
  p.risk.classList.toggle('alert', tone === 'alert');
  p.risk.classList.toggle('danger', tone === 'danger');
  const crew = crewLine(roster);
  p.crew.textContent = crew;
  p.crew.hidden = !crew;
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
