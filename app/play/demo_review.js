// The Quarterly Performance Review for the Breakpoint demo (BP-27; demo-only: never merged to dev; the port
// deletes this file). After a wallet's first contract AAR, one panel inside the hub asks for an assessment on
// the Director's Tally form; the hub's Feedback chip opens the same form at any time. With TALLY_FORM_URL empty
// neither is drawn and nothing is counted. The form opens in a new tab with four values: wallet, way_in, genesis
// (from this tab's cached Genesis answer: yes, no or unknown) and missions (this campaign's contract AARs,
// counted here). Nothing here reads the server. Every string goes in through textContent.

import { store } from './api.js';
import { wayIn } from './demo_ways_in.js';

// The Director's form (Wiki/For_The_Director/Tally_Feedback_Form_Setup.md). Empty: no chip, no panel, no count.
const TALLY_FORM_URL = '';

// Wiki copy (BP-27).
const COPY = {
  chip: 'Feedback',
  title: 'Quarterly Performance Review',
  body: 'Command requests your assessment of this contract. About a minute. Candour is mandatory; retaliation is merely likely.',
  file: 'File my assessment',
  later: 'Not now',
};

// The chip sits in the hub's title bar between the title and the mute button (.mute-btn, play.css:494).
const CSS = `
.ops-review { position: absolute; top: 1.5cqw; right: 11cqw; padding: 1cqw 2.5cqw; font-family: var(--mono); font-size: 3.6cqw; color: var(--accent); background: rgba(11, 11, 12, 0.94); border: 0.3cqw solid var(--accent); border-radius: 1.5cqw; cursor: pointer; }
.review-panel { position: absolute; left: 6cqw; right: 6cqw; top: 30cqw; z-index: 5; padding: 4cqw; display: flex; flex-direction: column; gap: 3cqw; text-align: left; background: rgba(11, 11, 12, 0.97); border: 0.5cqw solid var(--accent); border-radius: 2cqw; }
.review-panel[hidden] { display: none; }
.review-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 5.5cqw; line-height: 1.15; color: var(--accent); }
.review-body { margin: 0; font-size: 3.9cqw; line-height: 1.4; }
`;

const FLAG = w => `mm.review.asked.${w}`;      // localStorage, per wallet: the panel has been drawn once
const COUNT = c => `mm.review.contracts.${c}`; // localStorage, per campaign: contract AARs seen

let hub = null;
let panel = null;
let armed = false; // a contract AAR since the last draw; the next hub show draws the panel

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
  const c = campaign();
  const n = c ? Number(readLocal(COUNT(c))) : 0;
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

// On every hub show or hide. Registered after demo_ops.js's watch(), so an end of run that replaces the hub
// on the same show has already hidden it here. The flag is set where the panel is drawn (RULE-134).
function drawPanel() {
  const w = wallet();
  if (!armed || !w || hub.hidden || readLocal(FLAG(w)) !== null) return;
  armed = false;
  panel.hidden = false;
  writeLocal(FLAG(w), '1');
}

// demo_ops.js's aarNote, for a contract fight's AAR only (a raid's or an ambush's never calls it).
export function countContract() {
  if (!TALLY_FORM_URL) return;
  const c = campaign();
  if (c) writeLocal(COUNT(c), String(contracts() + 1));
  armed = true;
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
}
