// demo-only (BP-10b): the merc base screen (Breakpoint §4d, stretch 1). Never ported to dev.
//
// Pure rendering for demo_ops.js: the four building slots, their art, cost and effect lines, and
// the moment a building goes up. It makes no calls and holds no state; demo_ops.js owns the
// board, the bank and every POST. The server sends only which buildings stand (`base`, four
// bools); the costs and the copy live here, and the harness pins the costs to the server's
// (Reviewer BP-10a CF1). Every string goes in through textContent.

// One row per building, in the order of the server's `base` keys.
export const BASE = [
  { id: 'radio_shack', name: 'Radio Shack', cost: 50,
    effect: 'Raid risk 3% a drill, down from 5%.',
    flavour: 'A scrap dish on a leaning pipe mast. It hears raiders coming.',
    done: "Radio Shack operational. Raiders get heard before they're seen." },
  { id: 'hobbyist_benches', name: 'Hobbyist Benches', cost: 120,
    effect: 'Heat 10 a drill, down from 15.',
    flavour: 'Scout drones and utility kits, mostly assembled. The drill head runs cooler.',
    done: 'Benches operational. The drill head runs cooler.' },
  { id: 'grease_pit', name: 'The Grease-Pit', cost: 80,
    effect: 'Ambush odds on the haul halved.',
    flavour: 'Light vehicles, slow manual repairs. Armoured enough to argue with an ambush.',
    done: 'Grease-Pit operational. Hauls ride armoured.' },
  { id: 'gunsmith', name: 'Gunsmith', cost: 80,
    effect: 'Ore per drill up a quarter.',
    flavour: 'Slug-rifles for the Crew. An armed Crew digs with conviction.',
    done: 'Gunsmith operational. The Crew digs harder.' },
];

// Booth copy, verbatim from the BP-10b prompt.
export const BASE_COPY = {
  button: 'Base',
  title: 'Company Base',
  holdings: 'Holdings: {total} ore',
  build: 'Build · {cost} ore',
  short: 'Need {n} more ore',
  out: "The team is out. Build when they're home.",
  planned: 'PLANNED',
  operational: 'OPERATIONAL',
  pop: '−{n} ore',
  close: 'Close',
  endBase: 'Holdings do not transfer. {total} ore and the base stay on the old books.',
  endBaseOut: 'Holdings do not transfer. The base stays on the old books; ore on site and in transit is written off.',
  endBaseOnly: 'Holdings do not transfer. The base stays on the old books.',
};

// Placeholder art (BP-10b): line drawings in the accent colour, one per building. Real sprites
// replace one url() each (and join the service worker's SHELL). The Radio Shack follows
// Building_Art_Style_Guide's T1 Intelligence Hub; the other three have no design yet.
const SVG_HEAD = "data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 64 64' fill='none' stroke='%23d9a441' stroke-width='2.5' stroke-linecap='round' stroke-linejoin='round'>";
const ART = {
  radio_shack: "<path d='M6 58h52'/><rect x='40' y='48' width='14' height='10'/><path d='M22 58l6-40M30 58l2-40'/><path d='M23 51h8M25 39h7M27 27h5'/><path d='M14 20a14 14 0 0 0 22-14'/><path d='M25 13l6 6'/><path d='M32 18l26 40' stroke-width='1'/>",
  hobbyist_benches: "<path d='M6 58h52'/><path d='M8 46h48M14 46v12M49 46v12'/><path d='M18 46v-6M40 46v-4'/><rect x='26' y='20' width='12' height='6'/><path d='M20 16l6 6M44 16l-6 6M20 30l6-4M44 30l-6-4'/><circle cx='20' cy='16' r='4'/><circle cx='44' cy='16' r='4'/><circle cx='20' cy='30' r='4'/><circle cx='44' cy='30' r='4'/>",
  grease_pit: "<path d='M6 58h52'/><path d='M6 26l26-14 26 14'/><path d='M10 24v34M54 24v34'/><path d='M18 48h28l-4-8H24z'/><circle cx='22' cy='52' r='4'/><circle cx='42' cy='52' r='4'/>",
  gunsmith: "<path d='M6 58h52'/><path d='M10 40h44M14 40v18M49 40v18'/><path d='M16 12v28M48 12v28'/><path d='M10 22h44M36 22l-4 6M20 22v-3'/><path d='M22 40v-5h8v5'/>",
};
const artCss = id => `.base-art-${id} { background-image: url("${SVG_HEAD}${ART[id]}</svg>"); }`;

export const BASE_CSS = `
.ops-base { position: absolute; top: 11.5cqw; left: 3cqw; padding: 1cqw 2.5cqw; font-family: var(--mono); font-size: 3.6cqw; color: var(--accent); background: rgba(11, 11, 12, 0.94); border: 0.3cqw solid var(--accent); border-radius: 1.5cqw; cursor: pointer; }
.ops-base[hidden], .base-sheet[hidden], .base-note[hidden], .base-build[hidden] { display: none; }
.base-sheet { position: absolute; left: 3cqw; right: 3cqw; top: 20cqw; bottom: 17cqw; padding: 3cqw; display: flex; flex-direction: column; gap: 1.5cqw; text-align: left; overflow-y: auto; background: rgba(11, 11, 12, 0.96); border: 0.4cqw solid var(--panel-edge); border-radius: 2cqw; }
.base-head { position: relative; display: flex; align-items: baseline; justify-content: space-between; gap: 2cqw; }
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
.base-build { width: auto; max-width: none; min-height: 8.5cqw; margin-top: 0.8cqw; padding: 0 2.5cqw; font-size: 3.6cqw; align-self: flex-start; }
.base-close { width: auto; max-width: none; min-height: 8.5cqw; padding: 0 3cqw; font-size: 3.6cqw; }
${BASE.map(b => artCss(b.id)).join('\n')}
@media (prefers-reduced-motion: reduce) {
  .base-art { transition: none; }
  .base-pop.shown, .base-slot.just .base-stamp { animation: none; }
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

// `onBuild(id)` and `onClose()` are demo_ops.js's; the sheet only calls them.
export function createBase(onBuild, onClose) {
  const node = el('section', 'base-sheet');
  node.hidden = true;
  const head = el('div', 'base-head');
  const close = el('button', 'btn base-close', BASE_COPY.close);
  close.type = 'button';
  close.addEventListener('click', () => onClose());
  head.append(el('h2', 'ops-title', BASE_COPY.title), close);
  const holdings = el('p', 'base-line');
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
    text.append(el('p', 'base-name', b.name), el('p', 'base-effect', b.effect), el('p', 'base-flavour', b.flavour), button);
    slot.append(pic, text);
    return { b, slot, stamp, button };
  });
  node.append(head, holdings, pop, note, ...slots.map(s => s.slot));
  return { node, holdings, pop, note, slots };
}

// `s`: { base, banked, out, held, rate } from demo_ops.js; `built`: the id this render's response
// built, or null. A building is bought once; while the team is out nothing can be built.
export function renderBase(v, s, built) {
  v.holdings.textContent = fill(BASE_COPY.holdings, { total: s.banked });
  let note = s.rate || (s.out ? BASE_COPY.out : '');
  for (const { b, slot, stamp, button } of v.slots) {
    const up = s.base[b.id] === true;
    slot.classList.toggle('built', up);
    stamp.textContent = up ? BASE_COPY.operational : BASE_COPY.planned;
    button.hidden = up;
    const short = b.cost - s.banked;
    button.textContent = !s.out && short > 0 ? fill(BASE_COPY.short, { n: short }) : fill(BASE_COPY.build, { cost: b.cost });
    button.disabled = up || s.out || short > 0 || s.held;
    if (up && b.id === built) {
      replay(slot, 'just');
      v.pop.textContent = fill(BASE_COPY.pop, { n: b.cost });
      replay(v.pop, 'shown');
      if (!s.rate) note = b.done;
    }
  }
  v.note.textContent = note;
  v.note.hidden = !note;
  v.node.hidden = false;
}

export function hideBase(v) {
  v.node.hidden = true;
}
