// The Demo Shop for the Breakpoint demo (BP-25; demo-only (ODQ-74 (d)): never merged to dev; the port
// deletes this file). The hub's Demo Shop button opens it. Buy enters today's checkout with the same call and
// the same `then` as before BP-25 (Not now and Continue land on the hub), so checkout.js and its exits are
// unchanged. BP-29a's small print, then GAP-90's line, sit under Buy. BP-45 (Director ruling 2026-11-11): the Field Pass,
// Portraits and How-it-is-paid-for boxes are gone; the money model lives on the main website. The screen is outside main.js's SCREENS, so it closes itself whenever another stage screen shows
// (a late read's show(), an error). Every string goes in through textContent.
// BP-43b-1: the Trading Post (BP-43 scope items 1, 2, 4). Its own frame, below Back; the devnet checkout keeps its
// own frame above it, so in-game credits and the test payment never share a frame or a button row. It reads
// demo_ops.js's view (the second argument of mountShop): the bank, held(), company.trade's prices and the last
// refusal. Its rows are redrawn by redrawShop() from demo_ops.js's refresh(), which rebuilds the frame's children
// only, so the screen keeps its scroll and an open row survives (clamped as BP-28b's card row was).
// BP-43b-2: the shelf's loot (scope item 3). One row per Trophy Shelf item, newest first, each priced by its own
// grade from company.trade.loot, with one confirm; equipped kit is never on the shelf, so it is never offered. A
// trade whose prices aren't all positive integers is shut, ore and loot together (Reviewer BP-43b-1 F1).

import { bankedLine } from './demo_dig.js'; // BP-43b-1: the bank line, the hub chip's own

// Wiki copy (BP-25). Canon: Wiki/GDD/Soulbound_NFT_Policy.md §1, §2a; the Business Plan's money model.
const COPY = {
  title: 'Demo Shop',
  buy: 'Test buying a campaign · $3.99 test USDC', // BP-29a (Wiki copy, feedback item 38): nothing is sold here
  buyNote: 'No real money. This uses test-network USDC so we can check the payment system works before launch.',
  wallet: "Your wallet will warn that it doesn't recognise this site. It's right: this demo runs on the test network and isn't in the dApp Store yet.",
  back: 'Back to the hub',
  // BP-43b-1 (Wiki copy; the Director's script pass may reword): the Trading Post. Sell's lines moved from the
  // Company card (BP-28b), unchanged.
  tradeTitle: 'Trading Post',
  tradeSub: 'Credits only. The quartermaster does not take card.',
  tradeShut: 'The Trading Post is shut. Try again in a moment.',
  sell: 'Sell ore',
  sellAsk: 'Sell {o} ore for {c} credits?',
  sellGo: 'Sell',
  sold: 'Sold {o} ore for {c} credits. Command thanks you for your liquidity.',
  oreShort: 'There is less ore in the bank than that. The figures are up to date now.',
  buyOre: 'Buy ore',
  buyAsk: 'Buy {o} ore for {c} credits?',
  buyGo: 'Buy',
  bought: 'Bought {o} ore for {c} credits. The quartermaster calls it a fair price. Nobody else does.',
  buyNone: 'Ore costs {p} credits each here. The bank holds less than that.',
  buyShort: 'There are fewer credits in the bank than that. The figures are up to date now.',
  less: '-10',
  more: '+10',
  cancel: 'Cancel',
  // BP-43b-2 (Wiki copy; the Director's script pass may reword): the shelf's loot. The gone line is the Company
  // card's voice for the same refusal (kitGone).
  lootItem: '{item} ({g})',
  lootSell: 'Sell for {c}',
  lootAsk: 'Sell {item} for {c} credits?',
  lootSold: "Sold {item} for {c} credits. It will be someone else's lucky charm now.",
  lootGone: 'That item has left the shelf. The list is up to date now.',
};

const CSS = `
.screen-shop { gap: 3cqw; overflow-y: auto; justify-content: flex-start; justify-content: safe center; }
.shop-pic { display: block; width: 100%; height: auto; aspect-ratio: 1080 / 392; border-radius: 1cqw; }
.shop-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 7cqw; line-height: 1.1; text-align: center; }
.shop-wallet, .shop-note { margin: 0; font-size: 3.6cqw; line-height: 1.4; color: var(--muted); text-align: center; }
.shop-buy { flex-shrink: 0; } /* BP-29a: the column overflows the stage, so flex shrink squeezed Buy to its min-height and a wrapped label spilled */
.shop-frame { width: 100%; border: 0.5cqw solid var(--accent); border-radius: 1.5cqw; background: var(--panel); padding: 4cqw; display: flex; flex-direction: column; gap: 2.5cqw; text-align: left; }
.shop-label { margin: 0; font-family: var(--display); font-weight: 700; font-size: 5cqw; color: var(--accent); }
.shop-body { margin: 0; width: 100%; font-size: 3.9cqw; line-height: 1.4; text-align: left; }
.shop-body.muted { color: var(--muted); }
.shop-act { width: 100%; max-width: none; margin-top: 0; min-height: 11cqw; font-size: 4.4cqw; }
.shop-row { display: flex; flex-wrap: wrap; gap: 1.5cqw; }
.shop-row .shop-act { flex: 1 1 0; width: auto; min-height: 8cqw; padding: 0 2cqw; font-size: 3.6cqw; }
.shop-loot { flex-wrap: nowrap; align-items: center; } /* BP-43b-2: the item line and its Sell for button side by side */
.shop-loot .shop-body { flex: 1 1 0; min-width: 0; width: auto; }
.shop-loot .shop-act { flex: 0 0 auto; }
`;

let section = null;
let deps = null;
let view = null;     // BP-43b-1: demo_ops.js's view (bank, held, trade, notice, sell, buy; BP-43b-2: trophies, sellLoot), registered at the mount
let tradeBox = null; // BP-43b-1: the Trading Post frame on screen, or null
let open = null;     // the open row: { kind: 'sell' | 'buy', n } (BP-43b-1), or { kind: 'loot', item: { item_name, quality_grade } } (BP-43b-2)
let done = null;     // the last trade's line: { kind, o, c }, or { kind: 'loot', item, c } (BP-43b-2)

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function button(className, text, onClick) {
  const b = el('button', className, text);
  b.type = 'button';
  b.addEventListener('click', onClick);
  return b;
}

const fill = (text, values) => text.replace(/\{(\w+)\}/g, (m, k) => (k in values ? String(values[k]) : m));

function act(text, disabled, onTap) {
  const b = button('btn shop-act', text, onTap);
  b.disabled = Boolean(disabled); // a disabled <button> gets no click, so a tap on it does nothing
  return b;
}

function close() {
  section.hidden = true;
  section.replaceChildren();
  tradeBox = null;
  open = null;
  done = null;
}

// BP-43b-1: the Trading Post's rows. Shut (one line, no bank, no rows) until an ops body is adopted and while the
// body carries no company.trade (a server without BP-43a, e.g. a rollback). Prices are the body's, never pinned here.
// BP-43b-2 (Reviewer BP-43b-1 F1): also shut unless every price is a positive integer: ore_sell, ore_buy and all five
// loot grades, checked once, so a bad price shuts ore and loot together and the rule never moves with the shelf.
// Sell ore shows while ore is banked; Buy ore always shows, disabled with a line when the bank can't pay for one.
// An open row: the ask with its price, -10 / +10 (1 to the most it can post, starting at all for a sale and at 10, or
// the most affordable if less, for a buy; no text box, so no phone keyboard) and the confirm / Cancel.
// BP-43b-2: then one row per shelf item, newest first (the Trophy Shelf card's order), priced by trade.loot[grade]
// (a lookup, never an iteration of trade.loot). Its open row is the ask and the confirm / Cancel. It is named by value
// (the POST names the item, and the server sells the first equal entry), so it opens on the first equal row and
// closes on a redraw whose shelf holds no equal entry. Every confirm is built here, on each draw (Reviewer BP-43b-1 F2).
const GRADES = ['Basic', 'Standard', 'Specialized', 'Superior', 'Elite']; // the client's own list (Reviewer BP-43a F1)
const priced = p => Number.isInteger(p) && p > 0;
const sameItem = (a, b) => a.item_name === b.item_name && a.quality_grade === b.quality_grade;

function tradeRows() {
  const trade = view && view.adopted() ? view.trade() : null;
  const head = [el('h2', 'shop-label', COPY.tradeTitle), el('p', 'shop-body muted', COPY.tradeSub)];
  if (!trade || !(priced(trade.ore_sell) && priced(trade.ore_buy) && Boolean(trade.loot) && GRADES.every(g => priced(trade.loot[g])))) {
    open = null;
    done = null;
    return head.concat(el('p', 'shop-body', COPY.tradeShut));
  }
  const ore = Math.max(0, Number(view.ore()) || 0);
  const credits = Math.max(0, Number(view.credits()) || 0);
  const price = { sell: trade.ore_sell, buy: trade.ore_buy };
  const most = { sell: ore, buy: Math.floor(credits / price.buy) };
  const shelf = Array.isArray(view.trophies()) ? view.trophies().slice().reverse() : [];
  if (open && open.kind === 'loot' && !shelf.some(t => sameItem(t, open.item))) open = null;
  if (open && open.kind !== 'loot' && most[open.kind] < 1) open = null;
  if (open && open.kind !== 'loot') open.n = Math.min(Math.max(1, open.n), most[open.kind]);
  const isHeld = view.held();
  const rows = head.concat(el('p', 'shop-body', bankedLine(ore, credits)));
  for (const kind of ['sell', 'buy']) {
    if (!open || open.kind !== kind) {
      if (kind === 'sell' && ore > 0) rows.push(act(COPY.sell, isHeld, () => { open = { kind, n: ore }; done = null; view.clearNotice(); drawTrade(); }));
      if (kind === 'buy') {
        rows.push(act(COPY.buyOre, isHeld || most.buy < 1, () => { open = { kind, n: Math.min(10, most.buy) }; done = null; view.clearNotice(); drawTrade(); }));
        if (most.buy < 1) rows.push(el('p', 'shop-body muted', fill(COPY.buyNone, { p: price.buy })));
      }
      continue;
    }
    const o = open.n;
    const p = price[kind];
    const step = d => () => { if (open) open.n = Math.min(most[kind], Math.max(1, open.n + d)); drawTrade(); };
    const steps = el('div', 'shop-row');
    steps.append(act(COPY.less, isHeld || o <= 1, step(-10)), act(COPY.more, isHeld || o >= most[kind], step(10)));
    const go = el('div', 'shop-row');
    const post = kind === 'sell' ? view.sell : view.buy;
    go.append(act(kind === 'sell' ? COPY.sellGo : COPY.buyGo, isHeld, () => { open = null; post(o, p, () => { done = { kind, o, c: o * p }; drawTrade(); }); }),
      act(COPY.cancel, false, () => { open = null; drawTrade(); }));
    rows.push(el('p', 'shop-body', fill(kind === 'sell' ? COPY.sellAsk : COPY.buyAsk, { o, c: o * p })), steps, go);
  }
  let askDrawn = false;
  for (const t of shelf) {
    const item = { item_name: t.item_name, quality_grade: t.quality_grade };
    const p = trade.loot[item.quality_grade];
    if (!askDrawn && open && open.kind === 'loot' && sameItem(item, open.item)) {
      askDrawn = true;
      const go = el('div', 'shop-row');
      go.append(act(COPY.sellGo, isHeld, () => { open = null; view.sellLoot(item, p, () => { done = { kind: 'loot', item: item.item_name, c: p }; drawTrade(); }); }),
        act(COPY.cancel, false, () => { open = null; drawTrade(); }));
      rows.push(el('p', 'shop-body', fill(COPY.lootAsk, { item: item.item_name, c: p })), go);
      continue;
    }
    const row = el('div', 'shop-row shop-loot');
    row.append(el('p', 'shop-body', fill(COPY.lootItem, { item: item.item_name, g: item.quality_grade })),
      act(fill(COPY.lootSell, { c: p }), isHeld, () => { open = { kind: 'loot', item }; done = null; view.clearNotice(); drawTrade(); }));
    rows.push(row);
  }
  if (done) rows.push(el('p', 'shop-body', done.kind === 'loot' ? fill(COPY.lootSold, { item: done.item, c: done.c }) : fill(done.kind === 'sell' ? COPY.sold : COPY.bought, { o: done.o, c: done.c })));
  const n = view.notice();
  if (n && n.target === 'shop' && n.action === 'sell' && n.kind === 'ore') rows.push(el('p', 'shop-body', COPY.oreShort));
  if (n && n.target === 'shop' && n.action === 'buy' && n.kind === 'short') rows.push(el('p', 'shop-body', COPY.buyShort));
  if (n && n.target === 'shop' && n.action === 'sell_loot' && n.kind === 'gone') rows.push(el('p', 'shop-body', COPY.lootGone));
  return rows;
}

function drawTrade() {
  if (tradeBox) tradeBox.replaceChildren(...tradeRows());
}

// BP-43b-1 (spec check F2): demo_ops.js's refresh() calls this beside renderCompany(), so an open Shop follows held()
// and every adopted body (a trade's own 200, its 409's re-read, a latch in that body). Never openShop(): the screen
// keeps its scroll.
export function redrawShop() {
  if (!section || section.hidden || !tradeBox) return;
  drawTrade();
}

export function openShop() {
  const pic = el('img', 'shop-pic');
  pic.alt = '';
  pic.addEventListener('error', () => pic.remove()); // a failed picture leaves the text whole
  pic.src = 'assets/art/scene_shop_banner.jpg';
  const buy = button('btn btn-primary shop-buy', COPY.buy, () => { close(); deps.offerCheckout(() => deps.enterHub()); });
  const back = button('btn shop-back', COPY.back, () => { close(); deps.enterHub(); });
  // BP-43b-1 (scope item 4; spec check F3, belowBackFirst): the checkout in its own frame, then Back, then the Trading Post.
  const pay = el('div', 'shop-frame shop-pay');
  pay.append(buy, el('p', 'shop-note', COPY.buyNote), el('p', 'shop-wallet', COPY.wallet));
  tradeBox = el('div', 'shop-frame shop-trade');
  open = null;
  done = null;
  if (view) view.clearNotice();
  drawTrade();
  for (const s of document.querySelectorAll('#stage > .screen')) s.hidden = s !== section;
  section.replaceChildren(pic, el('h1', 'shop-title', COPY.title), pay, back, tradeBox);
  section.scrollTop = 0;
}

// `d`: main.js's deps (via demo_ops.js); `v`: demo_ops.js's view for the Trading Post (BP-43b-1: adopted, ore, credits,
// held, trade, notice, clearNotice, sell, buy; BP-43b-2: trophies, sellLoot).
export function mountShop(d, v) {
  deps = d;
  view = v;
  const style = document.createElement('style');
  style.textContent = CSS;
  document.head.append(style);
  section = el('section', 'screen screen-shop');
  section.id = 'screen-shop';
  section.hidden = true;
  const stage = document.getElementById('stage');
  stage.append(section);
  // One screen at a time: while the shop is open, any other stage screen that shows closes it.
  new MutationObserver(records => {
    if (section.hidden) return;
    if (records.some(r => r.target !== section && r.target.parentElement === stage && r.target.classList.contains('screen') && !r.target.hidden)) close();
  }).observe(stage, { subtree: true, attributes: true, attributeFilter: ['hidden'] });
}
