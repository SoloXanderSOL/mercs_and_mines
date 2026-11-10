// demo-only (BP-10b, BP-11b-1): the merc base screen (Breakpoint §4d, stretch 1). Never ported to dev.
//
// Pure rendering for demo_ops.js: the five building slots, their art, cost and effect lines, and
// the moment a building goes up. It makes no calls and holds no state; demo_ops.js owns the
// board, the bank and every POST. The server sends which buildings stand (`base`, five bools) and
// the one going up (`construction`, BP-21b); the costs and the copy live here, and the harness pins the costs to the server's
// (Reviewer BP-10a CF1). Every string goes in through textContent.

import { bankedLine } from './demo_dig.js';

// One row per building, in the order of the server's `base` keys.
export const BASE = [
  { id: 'radio_shack', name: 'Radio Shack', cost: 50,
    effect: 'Raid risk 3% a drill, down from 5%.',
    flavour: 'A scrap dish on a leaning pipe mast. It hears raiders coming.',
    done: "Radio Shack operational. Raiders get heard before they're seen." },
  { id: 'hobbyist_benches', name: 'Medbay', cost: 120, // BP-33: the Benches renamed; the key stays
    effect: 'Patch up 20 credits a head, not 40.',
    flavour: 'A field clinic with a strong stomach. Heals the wounded at half the price.',
    done: 'Medbay open. Patch-ups at half price.' },
  { id: 'grease_pit', name: 'The Grease-Pit', cost: 80,
    effect: 'A lost ambush costs a quarter of the haul.',
    flavour: 'Light vehicles, slow manual repairs. Armoured enough to argue with an ambush.',
    done: 'Grease-Pit operational. Hauls ride armoured.' },
  { id: 'gunsmith', name: 'Gunsmith', cost: 100,
    effect: '+10 on every fight your mercs take.',
    flavour: 'Slug-rifles, re-bored and re-blessed. The mercs hit harder.',
    done: 'Gunsmith operational. The mercs hit harder.' },
  { id: 'basic_smelter', name: 'Basic Smelter', cost: 80,
    effect: 'Ore per drill up a quarter.',
    flavour: 'A furnace, a chute and a man with tongs. More of every load comes out as ore.',
    done: 'Smelter operational. Every drill yields more.' },
];

// Booth copy, verbatim from the BP-10b prompt.
export const BASE_COPY = {
  button: 'Base',
  title: 'Company Base',
  build: 'Build · {cost} ore',
  short: 'Need {n} more ore',
  out: 'A squad is out. Build when every squad is home.',
  planned: 'PLANNED',
  operational: 'OPERATIONAL',
  // BP-21b (Wiki copy): a paid building goes up over the demo's seconds, counted down after the tap and never
  // before it (BP-21a R6: no shipped string states a build time; the game's own run to minutes and hours).
  going: 'GOING UP',
  left: 'Ready in {n} s',
  due: 'Nearly up',
  oneAtATime: 'One building goes up at a time.',
  pop: '−{n} ore',
  close: 'Close',
  pick: '{name} · {cost} ore', // BP-35: the plot card's title
  medbay: 'Open the Medbay', // BP-33: a built Medbay's button
  endBase: 'Banked ore and credits do not transfer. The base stays on the old books.',
  endBaseOut: 'Banked ore and credits do not transfer. The base stays on the old books; ore on site and in transit is written off.',
  endBaseOnly: 'Banked ore and credits do not transfer. The base stays on the old books.',
};

// List art (BP-10b): line drawings in the accent colour, one per building, kept as the list's thumbnails.
// The painted sprites (art list #11-15, BP-22) stand on the plot above the list instead; see PLOT.
const SVG_HEAD = "data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 64 64' fill='none' stroke='%23d9a441' stroke-width='2.5' stroke-linecap='round' stroke-linejoin='round'>";
const ART = {
  radio_shack: "<path d='M6 58h52'/><rect x='40' y='48' width='14' height='10'/><path d='M22 58l6-40M30 58l2-40'/><path d='M23 51h8M25 39h7M27 27h5'/><path d='M14 20a14 14 0 0 0 22-14'/><path d='M25 13l6 6'/><path d='M32 18l26 40' stroke-width='1'/>",
  hobbyist_benches: "<path d='M6 58h52'/><path d='M10 58V30l22-14 22 14v28'/><path d='M32 30v18M23 39h18'/>", // BP-33: the Medbay's list icon
  grease_pit: "<path d='M6 58h52'/><path d='M6 26l26-14 26 14'/><path d='M10 24v34M54 24v34'/><path d='M18 48h28l-4-8H24z'/><circle cx='22' cy='52' r='4'/><circle cx='42' cy='52' r='4'/>",
  gunsmith: "<path d='M6 58h52'/><path d='M10 40h44M14 40v18M49 40v18'/><path d='M16 12v28M48 12v28'/><path d='M10 22h44M36 22l-4 6M20 22v-3'/><path d='M22 40v-5h8v5'/>",
  basic_smelter: "<path d='M6 58h52'/><path d='M14 58V32h26v26'/><path d='M21 32V18h12v14'/><path d='M24 18v-6M30 18v-9'/><path d='M21 52a6 6 0 0 1 12 0'/><path d='M40 38l14 9v11'/>",
};
const artCss = id => `.base-art-${id} { background-image: url("${SVG_HEAD}${ART[id]}</svg>"); }`;

// The base that grows (BP-22, art list #10-15): the plot is a band of Base_Plot_Empty (1008 x 1050, fence to
// gate), and each standing building's sprite sits on its pad. Boxes are [left, top, width] in % of that file
// (Wiki's bp22_encode.ps1). Sprites are never `.base-art-<id>`: the harness finds the list's slots by that class.
// BP-33: the Medbay keeps the Benches' key (the wire, the server, healPrice). BP-33b: its plot sprite is the
// Director's picture under its own name, building_medbay_t1.png (Wiki-encoded, RULE-136), not the Benches' PNG;
// its box below is BP-22's formula on the same top-right pad (Wiki's bp33b_encode.ps1).
const MEDBAY = 'hobbyist_benches';
const PLOT = 'assets/art/base_plot_band.jpg';
const SPRITE = {
  radio_shack: [13.86, 10.09, 25.79],
  hobbyist_benches: [58.41, 18.51, 22.91], // BP-33b: building_medbay_t1.png on the top-right pad
  grease_pit: [36.47, 38.29, 24.01],
  gunsmith: [16.74, 56.1, 22.91],
  basic_smelter: [58.92, 55.41, 22.39],
};

export const BASE_CSS = `
/* BP-40a-2a (item 4): .ops-base is a .btn-bar button in the hub's bottom bar (play.css), no longer on the map's top edge */
.ops-base[hidden], .base-sheet[hidden], .base-note[hidden], .base-build[hidden], .base-open[hidden] { display: none; }
.base-sheet { position: absolute; left: 3cqw; right: 3cqw; top: 20cqw; bottom: 17cqw; padding: 3cqw; display: flex; flex-direction: column; gap: 1.5cqw; text-align: left; overflow-y: auto; background: rgba(11, 11, 12, 0.96); border: 0.4cqw solid var(--panel-edge); border-radius: 2cqw; }
.base-head { position: sticky; top: calc(-3cqw - 1px); z-index: 1; display: flex; align-items: baseline; justify-content: space-between; gap: 2cqw; background: rgb(11, 11, 12); }
.base-line { margin: 0; font-family: var(--mono); font-size: 3.8cqw; color: var(--accent); }
.base-note { margin: 0; font-size: 3.6cqw; line-height: 1.35; }
.base-pop { position: absolute; right: 0; top: 6cqw; font-family: var(--display); font-weight: 800; font-size: 4.6cqw; color: var(--accent); opacity: 0; pointer-events: none; }
.base-pop.shown { animation: base-pop 0.9s ease-out; }
@keyframes base-pop { from { opacity: 1; transform: none; } to { opacity: 0; transform: translateY(-3cqw); } }
.base-slot { display: grid; grid-template-columns: 17cqw 1fr; gap: 2.5cqw; align-items: start; padding: 1.5cqw; border: 0.3cqw dashed var(--panel-edge); border-radius: 1.5cqw; }
.base-slot.built { border-style: solid; border-color: var(--accent); }
.base-pic { position: relative; width: 17cqw; height: 17cqw; }
.base-art { position: absolute; inset: 0; background: center / contain no-repeat; filter: grayscale(1); opacity: 0.35; transition: filter 0.4s ease-out, opacity 0.4s ease-out; }
.base-slot.built .base-art { filter: none; opacity: 1; }
.base-stamp { position: absolute; left: 0; right: 0; bottom: 0; text-align: center; font-family: var(--mono); font-size: 2.1cqw; color: var(--muted); background: rgba(0, 0, 0, 0.7); }
.base-slot.built .base-stamp { color: var(--accent); }
.base-slot.just .base-stamp { animation: base-slam 0.3s ease-out; }
@keyframes base-slam { from { transform: scale(1.6); opacity: 0; } to { transform: none; opacity: 1; } }
.base-text { display: flex; flex-direction: column; gap: 0.6cqw; min-width: 0; }
.base-name { margin: 0; font-family: var(--display); font-weight: 800; font-size: 4.4cqw; line-height: 1.1; }
.base-effect { margin: 0; font-size: 3.4cqw; line-height: 1.3; color: var(--accent); }
.base-flavour { margin: 0; font-size: 3.1cqw; line-height: 1.25; color: var(--muted); }
.base-build, .base-open { width: auto; max-width: none; min-height: 8.5cqw; margin-top: 0.8cqw; padding: 0 2.5cqw; font-size: 3.6cqw; align-self: flex-start; }
.base-close { width: auto; max-width: none; min-height: 8.5cqw; padding: 0 3cqw; font-size: 3.6cqw; }
.base-plot { position: relative; flex: none; }
.base-plot-img { display: block; width: 100%; height: auto; aspect-ratio: 1008 / 1050; border-radius: 1.5cqw; }
.base-plot-sprite { position: absolute; height: auto; pointer-events: none; }
.base-plot-sprite[hidden] { display: none; }
.base-plot-sprite.dim { filter: grayscale(1); opacity: 0.35; pointer-events: auto; cursor: pointer; } /* BP-35: an empty pad shows its building, dimmed; tap it to build */
.base-plot-sprite.just { animation: base-rise 0.4s ease-out; }
@keyframes base-rise { from { transform: scale(0.8); opacity: 0.35; } to { transform: none; opacity: 1; } }
.base-pick { position: absolute; left: 2cqw; right: 2cqw; bottom: 2cqw; display: flex; flex-direction: column; gap: 1cqw; padding: 2cqw; text-align: left; background: rgba(11, 11, 12, 0.94); border: 0.3cqw solid var(--accent); border-radius: 1.5cqw; }
.base-pick.top { top: 2cqw; bottom: auto; }
.base-pick[hidden] { display: none; }
.base-pick-name { margin: 0; font-family: var(--display); font-weight: 800; font-size: 4.2cqw; line-height: 1.1; }
.base-pick-effect { margin: 0; font-size: 3.4cqw; line-height: 1.3; color: var(--accent); }
.base-pick-row { display: flex; gap: 2cqw; align-items: center; }
.base-pick-build, .base-pick-close { width: auto; max-width: none; min-height: 8.5cqw; padding: 0 2.5cqw; font-size: 3.6cqw; }
${BASE.map(b => artCss(b.id)).join('\n')}
@media (prefers-reduced-motion: reduce) {
  .base-art { transition: none; }
  .base-pop.shown, .base-slot.just .base-stamp, .base-plot-sprite.just { animation: none; }
}
`;

const fill = (text, values) => text.replace(/\{(\w+)\}/g, (m, k) => (k in values ? String(values[k]) : m));

function el(tag, className, text) {
  const node = document.createElement(tag);
  node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

// Restarts a one-shot CSS animation on `node`.
function replay(node, className) {
  node.classList.remove(className);
  void node.offsetWidth;
  node.classList.add(className);
}

// BP-35: draws the plot card for v.pick from that building's list slot (its Build label and disabled state, which
// renderBase has just set), or hides it when nothing is picked or the picked building stands. The texts are
// emptied while hidden, so no text search finds a second Build.
function drawPick(v) {
  const s = v.pick ? v.slots.find(x => x.b.id === v.pick) : null;
  const open = Boolean(s) && !s.slot.classList.contains('built');
  const c = v.card;
  c.node.hidden = !open;
  c.node.classList.toggle('top', open && SPRITE[s.b.id][1] > 45);
  c.name.textContent = open ? fill(BASE_COPY.pick, { name: s.b.name, cost: s.b.cost }) : '';
  c.effect.textContent = open ? s.b.effect : '';
  c.build.textContent = open ? s.button.textContent : '';
  c.build.disabled = !open || s.button.disabled;
  if (!open) v.pick = null;
}

// `onBuild(id)`, `onClose()` and `onMedbay()` (BP-33) are demo_ops.js's; the sheet only calls them.
export function createBase(onBuild, onClose, onMedbay) {
  const node = el('section', 'base-sheet');
  node.hidden = true;
  const head = el('div', 'base-head');
  const close = el('button', 'btn base-close', BASE_COPY.close);
  close.type = 'button';
  close.addEventListener('click', () => onClose());
  head.append(el('h2', 'ops-title', BASE_COPY.title), close);
  const bankedEl = el('p', 'base-line');
  const pop = el('span', 'base-pop');
  const note = el('p', 'base-note');
  const slots = BASE.map(b => {
    const slot = el('div', 'base-slot');
    const pic = el('div', 'base-pic');
    const stamp = el('span', 'base-stamp');
    pic.append(el('div', 'base-art base-art-' + b.id), stamp);
    const text = el('div', 'base-text');
    const button = el('button', 'btn btn-primary base-build');
    button.type = 'button';
    button.addEventListener('click', () => onBuild(b.id));
    const open = b.id === MEDBAY ? el('button', 'btn btn-primary base-open', BASE_COPY.medbay) : null; // BP-33
    if (open) {
      open.type = 'button';
      open.hidden = true;
      open.addEventListener('click', () => onMedbay());
    }
    text.append(el('p', 'base-name', b.name), el('p', 'base-effect', b.effect), el('p', 'base-flavour', b.flavour), button);
    if (open) text.append(open);
    slot.append(pic, text);
    return { b, slot, stamp, button, open };
  });
  const plot = el('div', 'base-plot');
  const ground = el('img', 'base-plot-img');
  ground.alt = '';
  ground.addEventListener('error', () => plot.remove());
  ground.src = PLOT;
  plot.append(ground);
  const sprites = {};
  for (const b of BASE) {
    const img = el('img', 'base-plot-sprite');
    const [left, top, width] = SPRITE[b.id];
    img.alt = '';
    img.hidden = true;
    img.dataset.building = b.id;
    img.style.left = `${left}%`;
    img.style.top = `${top}%`;
    img.style.width = `${width}%`;
    img.addEventListener('error', () => img.remove());
    img.addEventListener('click', () => { // BP-35: only an empty (dimmed) pad takes a tap
      if (!img.classList.contains('dim')) return;
      v.pick = b.id;
      drawPick(v);
    });
    img.src = b.id === MEDBAY ? 'assets/art/building_medbay_t1.png' : `assets/art/building_${b.id}.png`; // BP-33b
    plot.append(img);
    sprites[b.id] = img;
  }
  // BP-35: the plot card. Its Build sends the same onBuild as the list's; its close only drops the pick.
  const card = el('div', 'base-pick');
  card.hidden = true;
  const cardName = el('p', 'base-pick-name');
  const cardEffect = el('p', 'base-pick-effect');
  const cardBuild = el('button', 'btn btn-primary base-pick-build');
  cardBuild.type = 'button';
  cardBuild.addEventListener('click', () => { if (v.pick) onBuild(v.pick); });
  const cardClose = el('button', 'btn base-pick-close', '✕');
  cardClose.type = 'button';
  cardClose.setAttribute('aria-label', BASE_COPY.close);
  cardClose.addEventListener('click', () => { v.pick = null; drawPick(v); });
  const cardRow = el('div', 'base-pick-row');
  cardRow.append(cardBuild, cardClose);
  card.append(cardName, cardEffect, cardRow);
  plot.append(card);
  node.append(head, bankedEl, pop, note, plot, ...slots.map(s => s.slot));
  const v = { node, bankedEl, pop, note, slots, sprites, pick: null, card: { node: card, name: cardName, effect: cardEffect, build: cardBuild } };
  return v;
}

// `s`: { base, banked, credits, out, held, rate, going } from demo_ops.js; `going` (BP-21b): the building going up,
// { id, left } with whole seconds left on the server's clock, or null. `built`: the building this render shows
// standing for the first time (its moment), or null; `paid`: the building whose ore this render's build POST took
// (its pop), or null. A building is bought once; since BP-35 the demo's build time is 0, so it stands in its own
// call, and `going` is set only by a body that still carries `construction` (the fake's opt-in buildsecs); while a squad is out
// nothing can be built.
export function renderBase(v, s, built, paid) {
  v.bankedEl.textContent = bankedLine(s.banked, s.credits);
  let note = s.rate || (s.out ? BASE_COPY.out : '');
  let done = '';
  const lock = Boolean(s.going); // one at a time: the server answers `already building` (BP-21a)
  for (const { b, slot, stamp, button, open } of v.slots) {
    const up = s.base[b.id] === true;
    const going = !up && lock && s.going.id === b.id;
    slot.classList.toggle('built', up);
    v.sprites[b.id].hidden = false; // BP-35: every pad shows its building (BP-22: what stands, in colour)
    v.sprites[b.id].classList.toggle('dim', !up);
    stamp.textContent = up ? BASE_COPY.operational : going ? BASE_COPY.going : BASE_COPY.planned;
    button.hidden = up;
    if (open) open.hidden = !up; // BP-33: only a standing Medbay opens
    const short = b.cost - s.banked;
    if (going) button.textContent = s.going.left > 0 ? fill(BASE_COPY.left, { n: s.going.left }) : BASE_COPY.due;
    else button.textContent = !s.out && short > 0 ? fill(BASE_COPY.short, { n: short }) : fill(BASE_COPY.build, { cost: b.cost });
    button.disabled = up || going || lock || s.out || short > 0 || s.held;
    if (b.id === paid) {
      v.pop.textContent = fill(BASE_COPY.pop, { n: b.cost });
      replay(v.pop, 'shown');
    }
    if (up && b.id === built) {
      replay(slot, 'just');
      replay(v.sprites[b.id], 'just'); // BP-35: the sprite rises once
      done = b.done;
    }
  }
  if (!s.rate) note = done || note || (lock ? BASE_COPY.oneAtATime : '');
  v.note.textContent = note;
  v.note.hidden = !note;
  drawPick(v); // BP-35
  v.node.hidden = false;
}

// BP-40a-2a (BP-37a-2 check 15, RULE-114): a hub hide or return drops the sheet's one-shots and leaves the sheet as it
// was, so a pop or a rise never replays when the hub shows again (both orders: a beat running at the hide, a replay
// adopted while the hub was hidden).
export function quietBase(v) {
  v.pop.classList.remove('shown');
  for (const { slot } of v.slots) slot.classList.remove('just');
  for (const img of Object.values(v.sprites)) img.classList.remove('just');
}

export function hideBase(v) {
  v.node.hidden = true;
  // BP-21b rework (Reviewer BP-21b F1): display:none restarts a CSS animation when the sheet shows again, so a
  // class left on would replay the last pop and every slam on each reopen. replay() removes before it adds.
  v.pop.classList.remove('shown');
  for (const { slot } of v.slots) slot.classList.remove('just');
  for (const img of Object.values(v.sprites)) img.classList.remove('just'); // BP-35: as BP-21b F1
  v.pick = null; // BP-35: a reopen starts with no card
  drawPick(v);
}
