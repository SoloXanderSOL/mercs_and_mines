// The Demo Shop for the Breakpoint demo (BP-25; demo-only (ODQ-74 (d)): never merged to dev; the port
// deletes this file). The hub's Demo Shop button opens it. Buy enters today's checkout with the same call and
// the same `then` as before BP-25 (Not now and Continue land on the hub), so checkout.js and its exits are
// unchanged. GAP-90's line sits under Buy. The Breakpoint Genesis Field Pass (ODQ-70) has no mint button until
// BP-7. Nothing here reads the server: the Field Pass line reads the per-wallet Genesis answer that BP-3c-2
// cached (a 200 only), and no answer draws no line. The screen is outside main.js's SCREENS, so it closes
// itself whenever another stage screen shows (a late read's show(), an error). Every string goes in through
// textContent.

import { store } from './api.js';

// Wiki copy (BP-25). Canon: Wiki/GDD/Soulbound_NFT_Policy.md §1, §2a; the Business Plan's money model.
const COPY = {
  title: 'Demo Shop',
  buy: 'Buy a campaign · $3.99 test USDC',
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
};

const CSS = `
.screen-shop { gap: 3cqw; overflow-y: auto; justify-content: flex-start; justify-content: safe center; }
.shop-pic { display: block; width: 100%; height: auto; aspect-ratio: 1080 / 392; border-radius: 1cqw; }
.shop-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 7cqw; line-height: 1.1; text-align: center; }
.shop-wallet { margin: 0; font-size: 3.6cqw; line-height: 1.4; color: var(--muted); text-align: center; }
.shop-frame { width: 100%; border: 0.5cqw solid var(--accent); border-radius: 1.5cqw; background: var(--panel); padding: 4cqw; display: flex; flex-direction: column; gap: 2.5cqw; text-align: left; }
.shop-label { margin: 0; font-family: var(--display); font-weight: 700; font-size: 5cqw; color: var(--accent); }
.shop-h { margin: 0; width: 100%; font-family: var(--display); font-weight: 700; font-size: 5cqw; text-align: left; }
.shop-body { margin: 0; width: 100%; font-size: 3.9cqw; line-height: 1.4; text-align: left; }
`;

let section = null;
let deps = null;

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

function close() {
  section.hidden = true;
  section.replaceChildren();
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
  const frame = el('div', 'shop-frame');
  frame.append(el('h2', 'shop-label', COPY.passLabel), el('p', 'shop-body', COPY.passBody));
  if (holdsGenesis()) frame.append(el('p', 'shop-body shop-holds', COPY.passHolds));
  frame.append(el('p', 'shop-body', COPY.passSoon));
  const buy = button('btn btn-primary shop-buy', COPY.buy, () => { close(); deps.offerCheckout(() => deps.enterHub()); });
  const back = button('btn shop-back', COPY.back, () => { close(); deps.enterHub(); });
  for (const s of document.querySelectorAll('#stage > .screen')) s.hidden = s !== section;
  section.replaceChildren(pic, el('h1', 'shop-title', COPY.title), buy, el('p', 'shop-wallet', COPY.wallet), back, frame,
    el('h2', 'shop-h', COPY.portraitsTitle), el('p', 'shop-body', COPY.portraits),
    el('h2', 'shop-h', COPY.patronageTitle), el('p', 'shop-body', COPY.patronage));
  section.scrollTop = 0;
}

export function mountShop(d) {
  deps = d;
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
