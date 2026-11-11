// The Quarterly Performance Review for the Breakpoint demo (BP-27; demo-only: never merged to dev; the port
// deletes this file). After a wallet's first contract AAR, one panel inside the hub asks for an assessment on
// the Director's Tally form; the hub's Feedback chip opens the same form at any time. With TALLY_FORM_URL empty
// neither is drawn and nothing is counted. The form opens in a new tab with four values: wallet, way_in, genesis
// (from this tab's cached Genesis answer: yes, no or unknown) and missions (this campaign's contract AARs,
// counted here). Nothing here reads the server. Every string goes in through textContent.

import { store } from './api.js';
import { wayIn } from './demo_ways_in.js';

// The Director's form (Wiki/For_The_Director/Tally_Feedback_Form_Setup.md), live from BP-27b. It stays an absolute
// https://tally.so/r/ link: new URL throws on anything else. Empty would draw no chip and no panel and count nothing.
const TALLY_FORM_URL = 'https://tally.so/r/PRDNo5';

// Wiki copy (BP-27).
const COPY = {
  chip: 'Feedback',
  title: 'Quarterly Performance Review',
  body: 'Command requests your assessment of this contract. About a minute. Candour is mandatory; retaliation is merely likely.',
  file: 'File my assessment',
  later: 'Not now',
};

// The chip sits in the hub's title bar between the title and the mute button (.mute-btn in play.css).
const CSS = `
.ops-review { position: absolute; top: 1.5cqw; right: 11cqw; padding: 1cqw 2.5cqw; font-family: var(--mono); font-size: 3.6cqw; color: var(--accent); background: rgba(11, 11, 12, 0.94); border: 0.3cqw solid var(--accent); border-radius: 1.5cqw; cursor: pointer; }
.review-panel { position: absolute; left: 6cqw; right: 6cqw; top: 30cqw; z-index: 4; padding: 4cqw; display: flex; flex-direction: column; gap: 3cqw; text-align: left; background: rgba(11, 11, 12, 0.97); border: 0.5cqw solid var(--accent); border-radius: 2cqw; }
/* BP-27b: z-index 4, one under the Crew card's 5 in the same box (demo_dig.js .crew-card): a Crew loss is read first. */
.review-panel[hidden] { display: none; }
.review-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 5.5cqw; line-height: 1.15; color: var(--accent); }
.review-body { margin: 0; font-size: 3.9cqw; line-height: 1.4; }
`;

const FLAG = w => `mm.review.asked.${w}`;      // localStorage, per wallet: the panel has been drawn once
const COUNT = (w, c) => `mm.review.contracts.${w}.${c}`; // localStorage, per wallet and campaign (Reviewer BP-27 F3): contract AARs seen

let hub = null;
let panel = null;
let armed = null; // the wallet whose contract AAR armed the panel (Reviewer BP-27 F4); the next hub show draws it for that wallet only

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

function readLocal(key) {
  try { return localStorage.getItem(key); } catch { return null; }
}

function writeLocal(key, value) {
  try { localStorage.setItem(key, value); } catch { /* storage refused: the panel may ask again */ }
}

function wallet() {
  const s = store.session();
  return (s && s.wallet_address) || null;
}

function campaign() {
  const c = store.campaign();
  return (c && c.campaign_id) || null;
}

function contracts() {
  const w = wallet(), c = campaign();
  const n = w && c ? Number(readLocal(COUNT(w, c))) : 0;
  return Number.isInteger(n) && n > 0 ? n : 0;
}

// This tab's Genesis answer (BP-3c-2: cached from a 200 only): no answer is 'unknown', never 'no'.
function genesis(w) {
  const g = w ? store.genesis(w) : null;
  if (g && g.holds === true) return 'yes';
  if (g && g.holds === false) return 'no';
  return 'unknown';
}

function formUrl() {
  const w = wallet();
  const u = new URL(TALLY_FORM_URL);
  u.searchParams.set('wallet', w || '');
  u.searchParams.set('way_in', wayIn());
  u.searchParams.set('genesis', genesis(w));
  u.searchParams.set('missions', String(contracts()));
  return u.href;
}

// Synchronous inside the tap (user activation). noopener: the call returns null, opened or blocked alike.
function openForm() {
  window.open(formUrl(), '_blank', 'noopener');
}

// On every hub show or hide. Registered after demo_ops.js's watch(): an endOnHub end of run has already hidden the
// hub here; a contract's later end of run is handled by the screen-end observer (mountReview). The panel draws only for
// the wallet whose contract armed it (F4). The flag is set where the panel is drawn (RULE-134).
function drawPanel() {
  const w = wallet();
  if (!armed || armed !== w || hub.hidden || readLocal(FLAG(w)) !== null) return;
  armed = null;
  panel.hidden = false;
  writeLocal(FLAG(w), '1');
}

// demo_ops.js's aarNote, for a contract fight's AAR only (a raid's or an ambush's never calls it).
export function countContract() {
  if (!TALLY_FORM_URL) return;
  const w = wallet(), c = campaign();
  if (w && c) writeLocal(COUNT(w, c), String(contracts() + 1));
  armed = w; // F4: the wallet that earned it (null: nobody's)
}

export function mountReview(deps) {
  if (!TALLY_FORM_URL) return;
  const style = document.createElement('style');
  style.textContent = CSS;
  document.head.append(style);
  hub = deps.$('screen-hub');
  panel = el('div', 'review-panel');
  panel.hidden = true;
  const close = () => { panel.hidden = true; };
  panel.append(el('h2', 'review-title', COPY.title), el('p', 'review-body', COPY.body),
    button('btn btn-primary review-file', COPY.file, () => { close(); openForm(); }),
    button('btn review-later', COPY.later, close));
  hub.append(button('ops-review', COPY.chip, openForm), panel);
  new MutationObserver(drawPanel).observe(hub, { attributes: true, attributeFilter: ['hidden'] });
  // BP-27b (Reviewer BP-27 F1, R4): an end of run hides the panel and disarms it, so it never asks about "this
  // contract" in the next campaign. Two paths: endOfRun after the hub drew it (rv8x), and battle.js's showEndOfRun
  // after a Wipeout's AAR, which never shows the hub (rv9).
  const end = deps.$('screen-end');
  new MutationObserver(() => {
    if (end.hidden) return;
    panel.hidden = true; // rv8x: drawn on the hub, then the run ended
    armed = null; // rv9: armed by the AAR, the hub never shown
  }).observe(end, { attributes: true, attributeFilter: ['hidden'] });
}
