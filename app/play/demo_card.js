// The DEMO-001 card for the Breakpoint demo (demo-only (ODQ-66): never merged to dev; the port
// deletes this file). Public teaser text (`public: true`, Wiki/Story/DEMO-001_The_Seeker_Hoard.md),
// shipped verbatim. It sits between the first non-death AAR and the checkout offer, once per
// campaign: story → choice → outcome → the offer. Flavour only: nothing is stored or sent.
// Text-only frame (no illustration yet); every string goes in through textContent.

import { store } from './api.js';
import { setBeforeOffer } from './battle.js';

// A paragraph is a list of segments: a string, or { b } for a bold word.
const CARD = {
  title: 'The Seeker Hoard',
  opening: "Section came back from the mission with a slab of black glass they dug out of the dirt. Your lab Ape took it from them with tongs, and then, once it had read the inscription, with reverence.",
  body: [
    ['It is a pre-collapse device called a ', { b: 'Seeker' }, '. It still turns on.'],
    ['Inside is a wallet holding an ancient hoard of a digital currency called ', { b: 'SOL' }, '. According to the ledger, you are now its only owner. Of all of it. Anywhere.'],
    ['There are stories on it too. They tell of a mythical city called ', { b: 'London' }, ', and of a legendary gathering held there called ', { b: 'Breakpoint' }, '. One speaker is recorded in detail: a man with a calamitous blonde haircut, whose name has been lost to time. He spent his slot shuffling through his notes, losing his place, and talking at length about someone called Peppa Pig.'],
    ['The Ape has gone very quiet. It keeps murmuring "London" in an accent nobody on Gunmetal has ever been able to explain.'],
  ],
  continue: 'Continue',
  options: [
    { label: 'Hand it to G.U.N.', sub: 'Declare it. Claim the tax rebate.', outcome: 'G.U.N. thanks you for your honesty and confiscates the device. Your rebate comes to 4¢, less the 25% Corruption Tax. You receive 3¢, plus a receipt that is worth slightly more.' },
    { label: 'Give it to The Trust', sub: 'Blue Blood and brownie points.', outcome: 'A zeppelin collects it within the hour. The Trust files the Seeker in the Great Archive, sends you a canister of Blue Blood and adds one point to your standing. The point arrives with a note correcting the spelling in your covering letter.' },
    { label: 'Keep it', sub: 'Ditch $GCN. Bring back SOL.', outcome: 'You call a meeting to announce that SOL is the future and $GCN is finished. Attendance: you, and the Ape, who came for the biscuits. Nobody else owns any SOL. You point out that this makes you very early.' },
  ],
};

const CSS = `
.screen-card { gap: 3cqw; }
.card-frame { width: 100%; border: 0.5cqw solid var(--accent); border-radius: 1.5cqw; background: var(--panel); padding: 4cqw; display: flex; flex-direction: column; gap: 2.5cqw; text-align: left; }
.card-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 7cqw; line-height: 1.1; text-align: center; }
.card-body { margin: 0; font-size: 3.9cqw; line-height: 1.4; }
.card-pick { margin: 0; font-family: var(--display); font-weight: 700; font-size: 5.5cqw; color: var(--accent); }
.card-outcome { margin: 0; font-size: 4.6cqw; line-height: 1.4; }
.card-option { text-transform: none; padding: 2cqw 4cqw; min-height: 0; }
.card-option span { display: block; }
.card-sub { font-family: var(--body); font-weight: 400; font-size: 3.8cqw; color: var(--muted); }
`;

const seen = new Set(); // campaign ids whose card has opened, this page load only
let section = null;

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function paragraph(segments) {
  const p = el('p', 'card-body');
  for (const s of segments) p.append(typeof s === 'string' ? document.createTextNode(s) : el('strong', '', s.b));
  return p;
}

function button(className, text, onClick) {
  const b = el('button', className, text);
  b.type = 'button';
  b.addEventListener('click', onClick);
  return b;
}

// Shows `children` as the only screen in the stage.
function page(...children) {
  for (const s of document.querySelectorAll('#stage > .screen')) s.hidden = s !== section;
  section.replaceChildren(...children);
}

function showCard(ctx, next) {
  const camp = store.campaign();
  const id = camp && camp.campaign_id;
  if (!id || seen.has(id)) return next();
  seen.add(id);
  const name = typeof ctx.commander === 'string' ? ctx.commander.trim() : '';
  const opening = `${name ? `${name}'s` : 'Your'} ${CARD.opening}`;
  const done = () => { section.hidden = true; section.replaceChildren(); next(); };

  const outcome = option => {
    const frame = el('div', 'card-frame');
    frame.append(el('h1', 'card-title', CARD.title), el('p', 'card-pick', option.label), el('p', 'card-outcome', option.outcome));
    page(frame, button('btn btn-primary', CARD.continue, done));
  };
  const choice = () => page(el('h1', 'card-title', CARD.title), ...CARD.options.map(o => {
    const b = button('btn card-option', undefined, () => outcome(o));
    b.append(el('span', '', o.label), el('span', 'card-sub', o.sub));
    return b;
  }));
  const frame = el('div', 'card-frame');
  frame.append(el('h1', 'card-title', CARD.title), paragraph([opening]), ...CARD.body.map(paragraph));
  page(frame, button('btn btn-primary', CARD.continue, choice));
}

export function mountCard() {
  const style = document.createElement('style');
  style.textContent = CSS;
  document.head.append(style);
  section = el('section', 'screen screen-card');
  section.id = 'screen-card';
  section.hidden = true;
  document.getElementById('stage').append(section);
  setBeforeOffer(showCard);
}
