// The Demo Shop for the Breakpoint demo (BP-25; demo-only (ODQ-74 (d)): never merged to dev; the port
// deletes this file). The hub's Demo Shop button opens it. Buy enters today's checkout with the same call and
// the same `then` as before BP-25 (Not now and Continue land on the hub), so checkout.js and its exits are
// unchanged. BP-29a's small print, then GAP-90's line, sit under Buy. The Breakpoint Genesis Field Pass (ODQ-70) has no mint button until
// BP-7. The Field Pass line reads the per-wallet Genesis answer that BP-3c-2 cached (a 200 only), and no answer
// draws no line. The screen is outside main.js's SCREENS, so it closes itself whenever another stage screen shows
// (a late read's show(), an error). Every string goes in through textContent.
// BP-43b-1: the Trading Post (BP-43 scope items 1, 2, 4). Its own frame, below Back; the devnet checkout keeps its
// own frame above it, so in-game credits and the test payment never share a frame or a button row. It reads
// demo_ops.js's view (the second argument of mountShop): the bank, held(), company.trade's prices and the last
// refusal. Its rows are redrawn by redrawShop() from demo_ops.js's refresh(), which rebuilds the frame's children
// only, so the screen keeps its scroll and an open row survives (clamped as BP-28b's card row was).

import { store } from './api.js';
import { bankedLine } from './demo_dig.js'; // BP-43b-1: the bank line, the hub chip's own

// Wiki copy (BP-25). Canon: Wiki/GDD/Soulbound_NFT_Policy.md §1, §2a; the Business Plan's money model.
const COPY = {
  title: 'Demo Shop',
  buy: 'Try a test payment', // BP-29a (Wiki copy, feedback item 38): nothing is sold here
  buyNote: 'No real money. This uses test-network USDC so we can check the payment system works before launch.',
  wallet: "Your wallet will warn that it doesn't recognise this site. It's right: this demo runs on the test network and isn't in the dApp Store yet.",
  back: 'Back to the hub',
  passLabel: 'Breakpoint Genesis Field Pass',
  passBody: 'Free, one per Seeker Genesis Token. A soulbound NFT on the test network: welded to your wallet, never sold, never traded. It is not part of the Genesis Collection.',
  passHolds: 'Your Seeker holds a Genesis Token. You qualify.',
  passSoon: 'Minting opens soon. The clerk is still looking for the stamp.',
  portraitsTitle: 'Portraits, in the full game',
  portraits: 'Your G.U.N. personnel file has a photo. The standard one is free. The rare ones are soulbound 1-of-1 NFTs, one of each per Seeker Genesis Token, at a fixed price shown before you pay. They do nothing in a fight. They are there to be looked at.',
  patronageTitle: 'How it is paid for',
  patronage: 'With a Seeker Genesis Token the first campaign is free. Each one after costs $3.99. No subscriptions, no loot boxes, no ads, nothing to resell. The NFTs are patronage: if you like what we are building, here is something good to look at. Nothing more is promised, because nothing more exists.',
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
};

const CSS = `
.screen-shop { gap: 3cqw; overflow-y: auto; justify-content: flex-start; justify-content: safe center; }
.shop-pic { display: block; width: 100%; height: auto; aspect-ratio: 1080 / 392; border-radius: 1cqw; }
.shop-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 7cqw; line-height: 1.1; text-align: center; }
.shop-wallet, .shop-note { margin: 0; font-size: 3.6cqw; line-height: 1.4; color: var(--muted); text-align: center; }
.shop-buy { flex-shrink: 0; } /* BP-29a: the column overflows the stage, so flex shrink squeezed Buy to its min-height and a wrapped label spilled */
.shop-frame { width: 100%; border: 0.5cqw solid var(--accent); border-radius: 1.5cqw; background: var(--panel); padding: 4cqw; display: flex; flex-direction: column; gap: 2.5cqw; text-align: left; }
.shop-label { margin: 0; font-family: var(--display); font-weight: 700; font-size: 5cqw; color: var(--accent); }
.shop-h { margin: 0; width: 100%; font-family: var(--display); font-weight: 700; font-size: 5cqw; text-align: left; }
.shop-body { margin: 0; width: 100%; font-size: 3.9cqw; line-height: 1.4; text-align: left; }
.shop-body.muted { color: var(--muted); }
.shop-act { width: 100%; max-width: none; margin-top: 0; min-height: 11cqw; font-size: 4.4cqw; }
.shop-row { display: flex; flex-wrap: wrap; gap: 1.5cqw; }
.shop-row .shop-act { flex: 1 1 0; width: auto; min-height: 8cqw; padding: 0 2cqw; font-size: 3.6cqw; }
`;

let section = null;
let deps = null;
let view = null;     // BP-43b-1: demo_ops.js's view (bank, held, trade, notice, sell, buy), registered at the mount
let tradeBox = null; // BP-43b-1: the Trading Post frame on screen, or null
let open = null;     // BP-43b-1: the open row: { kind: 'sell' | 'buy', n }, the amount its confirm will post
let done = null;     // BP-43b-1: the last trade's line: { kind, o, c }

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
// Sell ore shows while ore is banked; Buy ore always shows, disabled with a line when the bank can't pay for one.
// An open row: the ask with its price, -10 / +10 (1 to the most it can post, starting at all for a sale and at 10, or
// the most affordable if less, for a buy; no text box, so no phone keyboard) and the confirm / Cancel.
function tradeRows() {
  const trade = view && view.adopted() ? view.trade() : null;
  const head = [el('h2', 'shop-label', COPY.tradeTitle), el('p', 'shop-body muted', COPY.tradeSub)];
  if (!trade) {
    open = null;
    done = null;
    return head.concat(el('p', 'shop-body', COPY.tradeShut));
  }
  const ore = Math.max(0, Number(view.ore()) || 0);
  const credits = Math.max(0, Number(view.credits()) || 0);
  const price = { sell: Math.max(0, Number(trade.ore_sell) || 0), buy: Math.max(0, Number(trade.ore_buy) || 0) };
  const most = { sell: ore, buy: price.buy > 0 ? Math.floor(credits / price.buy) : 0 };
  if (open && most[open.kind] < 1) open = null;
  if (open) open.n = Math.min(Math.max(1, open.n), most[open.kind]);
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
  if (done) rows.push(el('p', 'shop-body', fill(done.kind === 'sell' ? COPY.sold : COPY.bought, { o: done.o, c: done.c })));
  const n = view.notice();
  if (n && n.target === 'shop' && n.action === 'sell' && n.kind === 'ore') rows.push(el('p', 'shop-body', COPY.oreShort));
  if (n && n.target === 'shop' && n.action === 'buy' && n.kind === 'short') rows.push(el('p', 'shop-body', COPY.buyShort));
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

// The Genesis answer cached for this tab's wallet (BP-3c-2: written from a 200 only). No wallet or no answer: false.
function holdsGenesis() {
  const s = store.session();
  const w = s && s.wallet_address;
  const g = w ? store.genesis(w) : null;
  return Boolean(g && g.holds === true);
}

export function openShop() {
  const pic = el('img', 'shop-pic');
  pic.alt = '';
  pic.addEventListener('error', () => pic.remove()); // a failed picture leaves the text whole
  pic.src = 'assets/art/scene_shop_banner.jpg';
  const frame = el('div', 'shop-frame shop-pass'); // BP-43b-1: + shop-pass, so a reader can tell it from the other frames
  frame.append(el('h2', 'shop-label', COPY.passLabel), el('p', 'shop-body', COPY.passBody));
  if (holdsGenesis()) frame.append(el('p', 'shop-body shop-holds', COPY.passHolds));
  frame.append(el('p', 'shop-body', COPY.passSoon));
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
  section.replaceChildren(pic, el('h1', 'shop-title', COPY.title), pay, back, tradeBox, frame,
    el('h2', 'shop-h', COPY.portraitsTitle), el('p', 'shop-body', COPY.portraits),
    el('h2', 'shop-h', COPY.patronageTitle), el('p', 'shop-body', COPY.patronage));
  section.scrollTop = 0;
}

// `d`: main.js's deps (via demo_ops.js); `v`: demo_ops.js's view for the Trading Post (BP-43b-1: adopted, ore, credits,
// held, trade, notice, clearNotice, sell, buy).
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
