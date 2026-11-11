// demo-only (BP-10b, BP-11b-1): the merc base screen (Breakpoint §4d, stretch 1). Never ported to dev.
//
// Pure rendering for demo_ops.js: the five building slots, their art, cost and effect lines, and
// the moment a building goes up. It makes no calls and holds no state; demo_ops.js owns the
// board, the bank and every POST. The server sends which buildings stand (`base`, five bools) and
// the one going up (`construction`, BP-21b); the costs and the copy live here, and the harness pins the costs to the server's
// (Reviewer BP-10a CF1). Every string goes in through textContent.
// BP-40b: the plot pinch-zooms and drags inside its frame, like the map; the zoom is the sheet's own view state, kept
// across refreshes and reset to 1x whenever the sheet closes or the hub hides or returns.

import { bankedLine } from './demo_dig.js';

// One row per building, in the order of the server's `base` keys.
// BP-43b-2 (R-b; Director copy ruling 2026-11-11): `flavour` is the building's one line, verbatim, the same before and
// after it stands, read by both the list card and the building's box (drawPick). The Tent's is in BOX.
export const BASE = [
  { id: 'radio_shack', name: 'Radio Shack', cost: 50,
    effect: 'Raid risk 3% a drill, down from 5%.',
    flavour: 'A scrap dish on a leaning pipe mast. It hears raiders coming - sometimes. Can be a bit hit and miss.',
    done: "Radio Shack operational. Raiders get heard before they're seen." },
  { id: 'hobbyist_benches', name: 'Medbay', cost: 120, // BP-33: the Benches renamed; the key stays
    effect: 'Patch up 20 credits a head, not 40.',
    flavour: 'The surgeon was a vet even before the previous war. He is confident. His patients less so.',
    done: 'Medbay open. Patch-ups at half price.' },
  { id: 'grease_pit', name: 'The Grease-Pit', cost: 80,
    effect: 'A lost ambush costs a quarter of the haul.',
    flavour: 'Where vehicles go to be patched, parked and quietly blamed for the last convoy losing half its cargo.',
    done: 'Grease-Pit operational. Hauls ride armoured.' },
  { id: 'gunsmith', name: 'Gunsmith', cost: 100,
    effect: '+10 on every fight your mercs take.',
    flavour: 'Turns steel into tools for ending arguments quickly and not diplomatically.',
    done: 'Gunsmith operational. The mercs hit harder.' },
  { id: 'basic_smelter', name: 'Basic Smelter', cost: 80,
    effect: 'Ore per drill up a quarter.',
    flavour: 'Rock goes in, money comes out. After the G.U.N. has taken its cut, of course.',
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
// BP-40a-2b (BP-40 items 2-3): the Tent is painted into the band, with no id, pad or Build, so it is a clear button over
// the painting: [left, top, width, height] in % of the band (Wiki-measured on base_plot_band.jpg: canvas and sandbags).
const TENT = 'tent';
const TENT_BOX = [38.2, 3.3, 23.8, 20.5];
// BP-40b (Director ruling 2026-11-09): pinch-zoom and drag on the plot, like the map. The frame stays put; the ground,
// the Tent and the sprites zoom in one layer (.base-plot-zoom), and the tags and the box stay outside it, full size.
// The view is { s, tx, ty }: a scale from 1 (the whole plot) to MAX_ZOOM and a pan in % of the plot. TAP_SLOP is
// hexmap.js's. MAX_ZOOM: the art is about 1:1 at DPR 3, so every zoom is an upscale (spec check 4; a device line).
const TAP_SLOP = 8;
const MAX_ZOOM = 2.5;
// Each building's tag over its pad and its box's one button (demo_ops.js's onTarget sends it on). BP-43b-2 (R-b): the
// box's line is the building's BASE flavour; the Tent, which has no list card, keeps its name and its line here.
const BOX = {
  tent: { name: 'Operations Tent', tag: 'COMPANY', button: 'Your Company', flavour: 'Every permit on this base was signed and stamped here. The stamp is now broken. The paperwork continues unabated, just messier.' },
  radio_shack: { tag: 'INTEL', button: 'World Map' },
  hobbyist_benches: { tag: 'MEDBAY', button: 'Heal' },
  grease_pit: { tag: 'MISSIONS', button: 'Missions' },
  gunsmith: { tag: 'MERCS', button: 'Sections' },
  basic_smelter: { tag: 'CREW', button: 'Crew' },
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
.base-plot { position: relative; flex: none; overflow: hidden; border-radius: 1.5cqw; } /* BP-40b: the frame; the radius moved here from the ground */
.base-plot-img { display: block; width: 100%; height: auto; aspect-ratio: 1008 / 1050; }
/* BP-40b: the layer that zooms, in flow, so the ground still sets the plot's height. touch-action: none makes every
   finger on the art ours: one pans (or scrolls the sheet at 1x), two pinch (spec check 2 (b)). */
.base-plot-zoom { position: relative; transform-origin: 0 0; touch-action: none; }
.base-plot-zoom img { -webkit-user-drag: none; }
.base-plot-sprite { position: absolute; height: auto; cursor: pointer; } /* BP-40a-2b: built or not, a tap opens its box */
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
.base-pick-build, .base-pick-go, .base-pick-close { width: auto; max-width: none; min-height: 8.5cqw; padding: 0 2.5cqw; font-size: 3.6cqw; }
/* BP-40a-2b: the box's line, the Tent's clear button, and the tags in the map labels' style (canvas there: 800 12px, 3 px stroke) */
.base-pick-flavour { margin: 0; font-size: 3.1cqw; line-height: 1.25; color: var(--muted); }
.base-pick-build[hidden], .base-pick-effect[hidden] { display: none; }
.base-plot-tent { position: absolute; margin: 0; padding: 0; border: 0; background: none; cursor: pointer; }
.base-tag { position: absolute; transform: translate(-50%, calc(-100% - 0.5cqw)); padding: 0 0.5cqw; font-family: var(--display); font-weight: 800; font-size: 3cqw; line-height: 3.25cqw; color: #ffffff; -webkit-text-stroke: 0.75cqw #0b0b0c; paint-order: stroke fill; white-space: nowrap; pointer-events: none; }
.base-tag.in { transform: translate(-50%, 0.5cqw); } /* the Tent's sits on its roof, inside the plot */
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

// BP-40b: the art always covers the frame: s in [1, MAX_ZOOM], the pan in [100 - 100 s, 0] % on each axis.
function clampZoom(z) {
  z.s = Math.min(MAX_ZOOM, Math.max(1, z.s));
  const lo = 100 - 100 * z.s;
  z.tx = Math.min(0, Math.max(lo, z.tx));
  z.ty = Math.min(0, Math.max(lo, z.ty));
}

// BP-40b: writes the view: the layer's transform (none at 1x, as before the brick) and each tag over its building at
// full size, so text is never enlarged. At 1x every tag gets back the exact % it was built with.
function applyZoom(z) {
  const at1 = z.s === 1 && !z.tx && !z.ty;
  z.node.style.transform = at1 ? '' : `translate(${z.tx}%, ${z.ty}%) scale(${z.s})`;
  for (const t of z.tags) {
    t.node.style.left = `${z.tx + t.x * z.s}%`;
    t.node.style.top = `${z.ty + t.y * z.s}%`;
  }
}

// One frame queued at most, drawn only while a pointer moves (hexmap.js's requestFrame).
function frameZoom(z) {
  if (!z.frame) z.frame = requestAnimationFrame(() => { z.frame = 0; applyZoom(z); });
}

// Back to 1x with no gesture held. Writes nothing when the view is already at 1x with no frame pending.
function resetZoom(z) {
  const dirty = Boolean(z.frame) || z.s !== 1 || Boolean(z.tx) || Boolean(z.ty);
  if (z.frame) cancelAnimationFrame(z.frame);
  Object.assign(z, { s: 1, tx: 0, ty: 0, g: null, moved: false, swallow: false, frame: 0 });
  z.pts.clear();
  if (dirty) applyZoom(z);
}

// hexmap.js's gestureMove in % of the plot. One finger pans once zoomed; at 1x it scrolls the sheet instead, since the
// layer's touch-action takes the browser's own scroll away. Two fingers pinch about their midpoint.
function moveZoom(z, plot, sheet) {
  const g = z.g;
  const r = plot.getBoundingClientRect();
  if (!g || !r.width || !r.height) return;
  const pts = [...z.pts.values()];
  if (pts.length === 1 && g.pts.length === 1) {
    const dx = pts[0].x - g.pts[0].x, dy = pts[0].y - g.pts[0].y;
    if (g.s === 1) { sheet.scrollTop = g.top - dy; return; } // at 1x a finger on the art scrolls the sheet
    z.tx = g.tx + dx / r.width * 100;
    z.ty = g.ty + dy / r.height * 100;
  } else if (pts.length >= 2 && g.pts.length >= 2) {
    const d0 = Math.hypot(g.pts[0].x - g.pts[1].x, g.pts[0].y - g.pts[1].y) || 1;
    const d1 = Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y);
    // keep the plot point under the starting midpoint under the current midpoint
    const m0x = ((g.pts[0].x + g.pts[1].x) / 2 - r.left) / r.width * 100, m0y = ((g.pts[0].y + g.pts[1].y) / 2 - r.top) / r.height * 100;
    const m1x = ((pts[0].x + pts[1].x) / 2 - r.left) / r.width * 100, m1y = ((pts[0].y + pts[1].y) / 2 - r.top) / r.height * 100;
    z.s = Math.min(MAX_ZOOM, Math.max(1, g.s * d1 / d0));
    z.tx = m1x - (m0x - g.tx) / g.s * z.s;
    z.ty = m1y - (m0y - g.ty) / g.s * z.s;
  } else return;
  clampZoom(z);
  frameZoom(z);
}

// BP-40b: the plot's gestures. No pointer capture: a captured mouse click is retargeted off the sprite (spec check H1).
// The gesture re-anchors whenever the finger count changes, so 2 -> 1 fingers pans on without a jump. A gesture that
// moved past TAP_SLOP or had two fingers down eats the next real click in the layer (a mouse drag still clicks); a
// touch drag clicks nothing, so every pointerdown clears it (H3). A click from el.click() or a key (detail 0) is never
// eaten.
function bindZoom(z, plot, sheet) {
  const layer = z.node;
  const start = () => {
    z.g = { pts: [...z.pts.values()].map(p => ({ ...p })), s: z.s, tx: z.tx, ty: z.ty, top: sheet.scrollTop };
  };
  const end = e => {
    if (!z.pts.delete(e.pointerId)) return;
    if (z.moved) z.swallow = true;
    start();
  };
  layer.addEventListener('pointerdown', e => {
    z.pts.delete(e.pointerId); // a pointer we still hold lost its up (a mouse released outside the plot)
    if (!z.pts.size) { z.moved = false; z.down = { x: e.clientX, y: e.clientY }; }
    z.swallow = false; // H3: cleared at every press
    z.pts.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (z.pts.size > 1) z.moved = true;
    start();
  });
  layer.addEventListener('pointermove', e => {
    if ((e.pointerType === 'mouse' && !e.buttons) || !z.pts.has(e.pointerId)) return;
    z.pts.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (!z.moved && Math.hypot(e.clientX - z.down.x, e.clientY - z.down.y) > TAP_SLOP) z.moved = true;
    moveZoom(z, plot, sheet);
  });
  layer.addEventListener('pointerup', end);
  layer.addEventListener('pointercancel', end);
  layer.addEventListener('click', e => {
    if (!z.swallow || e.detail === 0) return;
    z.swallow = false;
    e.stopPropagation();
    e.preventDefault();
  }, true);
}

// BP-35, BP-40a-2b: draws the box for v.pick: the name, its line, its effect, Build while the building isn't standing
// (the list slot's label and disabled state, which renderBase has just set) and its one button; or hides it when
// nothing is picked. The Tent always stands and has no slot or effect. A box stays open when its building goes up
// (R9). The texts are emptied while hidden, so no text search finds a second Build.
function drawPick(v) {
  const box = v.pick ? BOX[v.pick] : null;
  const s = box && v.pick !== TENT ? v.slots.find(x => x.b.id === v.pick) : null;
  const open = Boolean(box) && (v.pick === TENT || Boolean(s));
  const up = open && (!s || s.slot.classList.contains('built'));
  const c = v.card;
  c.node.hidden = !open;
  c.node.classList.toggle('top', Boolean(s) && SPRITE[v.pick][1] > 45);
  c.name.textContent = open ? (s ? s.b.name : box.name) : '';
  c.flavour.textContent = open ? (s ? s.b.flavour : box.flavour) : ''; // BP-43b-2 (R-b): one source
  c.effect.textContent = s ? s.b.effect : '';
  c.effect.hidden = !s;
  c.build.textContent = open && !up ? s.button.textContent : '';
  c.build.hidden = up;
  c.build.disabled = !open || up || s.button.disabled;
  c.go.textContent = open ? box.button : '';
  c.go.disabled = !open || (v.pick === 'grease_pit' && v.fight); // R8: as the bar's Missions
  if (!open) v.pick = null;
}

// `onBuild(id)`, `onClose()`, `onMedbay()` (BP-33) and `onTarget(id)` (BP-40a-2b: a box's one button, the Tent's id
// 'tent') are demo_ops.js's; the sheet only calls them.
export function createBase(onBuild, onClose, onMedbay, onTarget) {
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
  const layer = el('div', 'base-plot-zoom'); // BP-40b: what zooms; the tags and the box stay on the plot
  const ground = el('img', 'base-plot-img');
  ground.alt = '';
  ground.addEventListener('error', () => plot.remove());
  ground.src = PLOT;
  layer.append(ground);
  plot.append(layer);
  // BP-40a-2b (R1): the Tent's clear button goes in before the sprites, so a building keeps its own tap where their
  // boxes overlap (the Radio Shack's and the Medbay's; spec check §5).
  const tent = el('button', 'base-plot-tent');
  tent.type = 'button';
  tent.setAttribute('aria-label', BOX[TENT].name);
  tent.style.left = `${TENT_BOX[0]}%`;
  tent.style.top = `${TENT_BOX[1]}%`;
  tent.style.width = `${TENT_BOX[2]}%`;
  tent.style.height = `${TENT_BOX[3]}%`;
  tent.addEventListener('click', () => { v.pick = TENT; drawPick(v); });
  layer.append(tent);
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
    img.addEventListener('error', () => { // BP-40b (Reviewer BP-40a-2b F3): its tag goes with it
      img.remove();
      const tag = plot.querySelector(`.base-tag[data-building="${b.id}"]`);
      if (tag) tag.remove();
    });
    const pick = () => { // BP-35, BP-40a-2b: every pad takes a tap and opens its box, built or not
      v.pick = b.id;
      drawPick(v);
    };
    img.addEventListener('click', pick);
    // BP-40b (Reviewer BP-40a-2b F4): a pad is a button to a keyboard too. Enter and Space pick it directly, never
    // through img.click(), so no gesture can eat a key.
    img.tabIndex = 0;
    img.setAttribute('role', 'button');
    img.setAttribute('aria-label', b.name);
    img.addEventListener('keydown', e => {
      if (e.key !== 'Enter' && e.key !== ' ') return;
      e.preventDefault();
      pick();
    });
    img.src = b.id === MEDBAY ? 'assets/art/building_medbay_t1.png' : `assets/art/building_${b.id}.png`; // BP-33b
    layer.append(img);
    sprites[b.id] = img;
  }
  // BP-40a-2b (item 2): a one-word tag over every building, the Tent's on its roof; under the box, never taking a tap.
  const tags = []; // BP-40b: each tag's place at 1x, in % of the plot, for applyZoom
  for (const id of [TENT, ...BASE.map(b => b.id)]) {
    const tag = el('span', id === TENT ? 'base-tag in' : 'base-tag', BOX[id].tag);
    tag.dataset.building = id;
    const [left, top, width] = id === TENT ? TENT_BOX : SPRITE[id];
    tag.style.left = `${left + width / 2}%`;
    tag.style.top = `${top}%`;
    plot.append(tag);
    tags.push({ node: tag, x: left + width / 2, y: top });
  }
  // BP-40b: the view, bound once; renderBase never touches it, so a refresh in the middle of a gesture keeps the zoom.
  const zoom = { node: layer, tags, s: 1, tx: 0, ty: 0, pts: new Map(), g: null, down: { x: 0, y: 0 }, moved: false, swallow: false, frame: 0 };
  bindZoom(zoom, plot, node);
  // BP-35, BP-40a-2b (item 3): the building's box. Its Build sends the same onBuild as the list's; its one button drops
  // the box first, so a return to the base never finds it open (R7), then goes; its close only drops the pick.
  const card = el('div', 'base-pick');
  card.hidden = true;
  const cardName = el('p', 'base-pick-name');
  const cardFlavour = el('p', 'base-pick-flavour');
  const cardEffect = el('p', 'base-pick-effect');
  const cardBuild = el('button', 'btn btn-primary base-pick-build');
  cardBuild.type = 'button';
  cardBuild.addEventListener('click', () => { if (v.pick) onBuild(v.pick); });
  const cardGo = el('button', 'btn btn-primary base-pick-go');
  cardGo.type = 'button';
  cardGo.addEventListener('click', () => {
    const id = v.pick;
    if (!id) return;
    v.pick = null;
    drawPick(v);
    onTarget(id);
  });
  const cardClose = el('button', 'btn base-pick-close', '✕');
  cardClose.type = 'button';
  cardClose.setAttribute('aria-label', BASE_COPY.close);
  cardClose.addEventListener('click', () => { v.pick = null; drawPick(v); });
  const cardRow = el('div', 'base-pick-row');
  cardRow.append(cardBuild, cardGo, cardClose);
  card.append(cardName, cardFlavour, cardEffect, cardRow);
  plot.append(card);
  node.append(head, bankedEl, pop, note, plot, ...slots.map(s => s.slot));
  const v = { node, bankedEl, pop, note, slots, sprites, zoom, pick: null, fight: false, card: { node: card, name: cardName, flavour: cardFlavour, effect: cardEffect, build: cardBuild, go: cardGo } };
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
  v.fight = Boolean(s.fight); // BP-40a-2b (R8): the Grease Pit's Missions is shut while a raid or an ambush waits
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
  resetZoom(v.zoom); // BP-40b: the hub hides or returns at 1x
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
  resetZoom(v.zoom); // BP-40b: and at 1x
}
