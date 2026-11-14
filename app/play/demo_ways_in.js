// Two ways in for the Breakpoint demo (ODQ-69, amended 2026-11-07; demo-only: never merged to dev,
// where canon stays Seeker-only and MWA-only).
//
// 1. "Sign in on your Solana Seeker" runs today's MWA sign-in unchanged. The button tapped is
//    recorded as the way in.
// 2. "Just try it": a throwaway Ed25519 wallet made with WebCrypto on this phone. It signs the
//    server's challenge itself through the same two sign-in posts, so the server is unchanged.
//
// Two stores (BP-26). Per tab (sessionStorage): the way this tab signed in (mm.way_in), which the
// checkout reads. Kept on this phone (localStorage): the throwaway's private JWK
// (mm.throwaway.key) and the remembered way (mm.way_in = 'throwaway', written only after a
// throwaway verify succeeds). store.clearAll() touches neither store's keys, so a 401 keeps the
// key and the next sign-in is the same wallet. A fresh tab with a remembered throwaway signs
// itself back in (resumeRemembered, from boot() only, never from a 401).
//
// The checkout (BP-3c-2b) is real on the Seeker way in only: the throwaway gets its walkthrough.

import { postChallenge, postVerify, store } from './api.js';
import { base58 } from './wallet.js';
import { setWalkthroughWhen } from './checkout.js';
import { mountCard } from './demo_card.js';

const WAY_KEY = 'mm.way_in';
const THROWAWAY_KEY = 'mm.throwaway.key';
const DESK_KEY = 'mm.demo.desk_note'; // BP-48 (R-j): per tab (sessionStorage), '1' once Carry on here was tapped
const DESK_QUERY = '(pointer: fine) and (hover: hover) and (min-width: 900px)'; // a computer: never the user-agent
const DESK_QR = 'assets/art/qr_play_mercsandmines.png';

// Booth copy (BP-3c-2; BP-26 drafts, the Director's script pass may change them).
const COPY = {
  waySeeker: 'Sign in on your Solana Seeker',
  wayExplainer: 'Mercs & Mines launches on the Solana Seeker, and this demo tests its sign-in. No Seeker? Any phone plays with a throwaway wallet. Signing in spends nothing.',
  wayThrowaway: 'Just try it (no wallet needed)',
  wayThrowawayNote: "We'll make a throwaway wallet on this phone. It holds nothing. Come back on the same phone within two days and your company should still be waiting.",
  throwawayUnsupported: "This browser can't make a throwaway wallet. Try Chrome, or Safari on iOS 17 or later.",
  genesisThrowawayTitle: 'A wallet made for the occasion.',
  genesisThrowawayBody: 'It holds no Genesis Token, no funds and no grudges. It stays on this phone, so your company should still be here when you get back.',
  startOver: 'Start over as a new player',
  startOverAsk: 'This phone forgets your current company. Start over?',
  startOverYes: 'Yes, start over',
  startOverNo: 'No, keep it',
  resumeWorking: 'Signing you back in...',
  resumeFailed: "We couldn't sign you back in. Choose a way in below.",
  signInAnother: 'Not you? Sign in another way',
  // BP-48 (R-j; the Director's draft, tightened; the script pass may reword): the note for computers.
  deskHeading: 'BUILT FOR THE FIELD',
  deskLine: "You can play this on your computer, but the demo is laid out for a phone. If you can, we'd recommend playing on mobile.",
  deskScan: 'Scan this with your phone to open the demo in its browser.',
  deskType: 'Or type the address in yourself: mercsandmines.com/play',
  deskCarryOn: 'Carry on here',
  deskQrAlt: 'QR code: mercsandmines.com/play',
};

const CSS = `
.way-link { background: none; border: none; padding: 0; margin: 0 0.5em; font: inherit; color: var(--accent); text-decoration: underline; cursor: pointer; }
.way-link:disabled { opacity: 0.5; cursor: default; }
/* BP-48 (R-j): the note for computers, a card over the title's content inside the stage */
.desk-note { position: absolute; inset: 0; z-index: 2; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 3.5cqw; padding: 8cqw 6cqw; text-align: center; background: var(--bg); }
.desk-note[hidden] { display: none; }
.desk-title { margin: 0; font-family: var(--display); font-weight: 800; font-size: 7cqw; line-height: 1.1; letter-spacing: 0.04em; color: var(--accent); }
.desk-note .explainer { margin: 0; }
.desk-qr { width: 44cqw; height: 44cqw; image-rendering: pixelated; background: #fff; }
`;

// ── per-tab storage (a failure leaves the value in memory only) ────────────
const memory = new Map();

function readTab(key) {
  try {
    const v = sessionStorage.getItem(key);
    if (v !== null) return v;
  } catch { /* fall through to memory */ }
  return memory.has(key) ? memory.get(key) : null;
}

function writeTab(key, value) {
  memory.set(key, value);
  try { sessionStorage.setItem(key, value); } catch { /* memory only */ }
}

function removeTab(key) {
  memory.delete(key);
  try { sessionStorage.removeItem(key); } catch { /* memory only */ }
}

// ── kept on this phone (BP-26; a failure leaves the value in memory only) ──
const kept = new Map();

function readKept(key) {
  try {
    const v = localStorage.getItem(key);
    if (v !== null) return v;
  } catch { /* fall through to memory */ }
  return kept.has(key) ? kept.get(key) : null;
}

function writeKept(key, value) {
  kept.set(key, value);
  try { localStorage.setItem(key, value); } catch { /* memory only */ }
}

function removeKept(key) {
  kept.delete(key);
  try { localStorage.removeItem(key); } catch { /* memory only */ }
}

// The stored throwaway: kept on the phone, else a per-tab key from a tab open across BP-26's deploy.
function storedKey() {
  return readKept(THROWAWAY_KEY) || readTab(THROWAWAY_KEY);
}

// The way this tab signed in, for BP-3c-2b's checkout: 'seeker' when absent or 'seeker', else
// 'throwaway'. A legacy 'wallet' (a tab open across BP-26's deploy) keeps the walkthrough it had.
export function wayIn() {
  const v = readTab(WAY_KEY);
  return v === null || v === 'seeker' ? 'seeker' : 'throwaway';
}

// ── the throwaway wallet ────────────────────────────────────────────────────
const ED25519 = { name: 'Ed25519' };

function base64UrlToBytes(s) {
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/').padEnd(Math.ceil(s.length / 4) * 4, '=');
  return Uint8Array.from(atob(b64), c => c.charCodeAt(0));
}

// The stored key, re-imported (its JWK carries the public key as `x`).
async function importStored(raw) {
  const jwk = JSON.parse(raw);
  const pub = base64UrlToBytes(jwk.x);
  if (pub.length !== 32) throw new Error('stored key is not an Ed25519 key');
  const privateKey = await crypto.subtle.importKey('jwk', jwk, ED25519, false, ['sign']);
  return { privateKey, address: base58(pub) };
}

async function generate() {
  const pair = await crypto.subtle.generateKey(ED25519, true, ['sign', 'verify']);
  const jwk = await crypto.subtle.exportKey('jwk', pair.privateKey);
  const pub = new Uint8Array(await crypto.subtle.exportKey('raw', pair.publicKey));
  writeKept(THROWAWAY_KEY, JSON.stringify(jwk));
  return { privateKey: pair.privateKey, address: base58(pub) };
}

async function getOrCreate() {
  const raw = storedKey();
  if (!raw) return generate();
  const key = await importStored(raw);
  writeKept(THROWAWAY_KEY, raw); // a per-tab key from before BP-26 is kept on the phone from now on
  return key;
}

// Resolves with {privateKey, address}, or null when this browser can't make one. A failure
// (no WebCrypto, no Ed25519, a corrupt stored key) removes the stored key and tries once more.
// Only a tap runs this: the automatic resume imports and never generates.
async function throwawayKey() {
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      if (!globalThis.crypto || !crypto.subtle) return null;
      return await getOrCreate();
    } catch {
      removeKept(THROWAWAY_KEY);
      removeTab(THROWAWAY_KEY);
    }
  }
  return null;
}

// The 64 raw signature bytes over the challenge's UTF-8, in base58 (no slicing: there is no
// message prefix). Never retried: verify consumes the challenge.
async function throwawaySignIn(key) {
  const { challenge } = await postChallenge(key.address);
  const sig = await crypto.subtle.sign(ED25519, key.privateKey, new TextEncoder().encode(challenge));
  return postVerify(key.address, challenge, base58(new Uint8Array(sig)));
}

// ── the automatic resume (BP-26) ────────────────────────────────────────────
let deps = null;      // mountWaysIn's { $, show, toSignIn, enterCampaign }
let deskCard = null;  // BP-48: the note's card, built on its first show
let deskShown = false; // BP-48: shown in this page load (so a storage failure still shows it once per load)
let resumed = false;  // this tab's session came from the resume: the reveal offers another way in

// boot() calls this when no session is held. True when a resume has started (it shows its own
// screens); false when this phone remembers no throwaway, and boot shows the title as before.
// BP-48 (R-j): the "built for phones" note for computers, a card inside #screen-title. main.js calls it at the
// first-visit title only (no session, no remembered throwaway): never on a resume, an expired session, a stored
// session or a return from the wallet or the checkout. It shows on a fine pointer that hovers in a wide window, once
// per visit: Carry on here writes the key in its own handler. A storage failure shows it once per page load.
export function deskNote() {
  if (!deps || deskShown) return;
  let seen = false;
  try { seen = sessionStorage.getItem(DESK_KEY) === '1'; } catch { /* storage refused: once per page load */ }
  if (seen || typeof matchMedia !== 'function' || !matchMedia(DESK_QUERY).matches) return;
  if (!deskCard) deskCard = buildDeskNote();
  deskCard.hidden = false;
  deskShown = true;
}

function buildDeskNote() {
  const card = element('div', 'desk-note', '');
  card.id = 'desk-note';
  card.hidden = true;
  const qr = document.createElement('img');
  qr.className = 'desk-qr';
  qr.src = DESK_QR;
  qr.alt = COPY.deskQrAlt;
  const carry = element('button', 'btn btn-primary', COPY.deskCarryOn);
  carry.type = 'button';
  carry.id = 'btn-desk-carry';
  carry.addEventListener('click', () => {
    try { sessionStorage.setItem(DESK_KEY, '1'); } catch { /* not kept: the next page load asks again */ }
    card.hidden = true;
  });
  card.append(element('p', 'desk-title', COPY.deskHeading), element('p', 'explainer', COPY.deskLine),
    element('p', 'explainer', COPY.deskScan), qr, element('p', 'explainer', COPY.deskType), carry);
  deps.$('screen-title').append(card);
  return card;
}

export function resumeRemembered() {
  if (!deps || readKept(WAY_KEY) !== 'throwaway') return false;
  const raw = storedKey();
  if (!raw) return false;
  resume(raw);
  return true;
}

async function resume(raw) {
  const { $, show, toSignIn, enterCampaign } = deps;
  $('working-text').textContent = COPY.resumeWorking;
  show('screen-working');
  let token;
  try {
    const key = await importStored(raw); // import only, never a new key: one that won't import stays for "Start over"
    token = await throwawaySignIn(key);
  } catch {
    return toSignIn(COPY.resumeFailed);
  }
  store.setSession(token);
  store.clearCampaign(); // as onSignIn does: the join names the campaign
  writeTab(WAY_KEY, 'throwaway');
  resumed = true;
  return enterCampaign(0); // no Genesis screen on a resume: straight to the join and the reveal
}

// ── the sign-in screen ──────────────────────────────────────────────────────
function element(tag, className, text) {
  const node = document.createElement(tag);
  node.className = className;
  node.textContent = text;
  return node;
}

function linkButton(text) {
  const b = element('button', 'way-link', text);
  b.type = 'button';
  return b;
}

// Relabels the Seeker button and the explainer, and adds the throwaway after it, with "Start
// over" under its note. Call it after main.js has filled its copy and before it wires its own
// handlers, so the way is recorded before the Seeker button's sign-in starts. `btn-signin` is
// disabled while any sign-in runs (main.js), and the others follow it; all are disabled while the
// throwaway key is prepared, so no other sign-in can start then (Reviewer BP-3c-2 F1/F2).
export function mountWaysIn({ $, onSignIn, show, toSignIn, enterCampaign }) {
  deps = { $, show, toSignIn, enterCampaign };
  const style = document.createElement('style');
  style.textContent = CSS;
  document.head.append(style);

  const seeker = $('btn-signin');
  seeker.textContent = COPY.waySeeker;
  $('signin-explainer').textContent = COPY.wayExplainer;

  const throwaway = element('button', 'btn', COPY.wayThrowaway);
  throwaway.type = 'button';
  throwaway.id = 'btn-signin-throwaway';
  const throwawayNote = element('p', 'explainer way-note', COPY.wayThrowawayNote);
  const startOver = linkButton(COPY.startOver);
  startOver.id = 'btn-start-over';
  const startOverLine = element('p', 'explainer way-note', '');
  startOverLine.id = 'way-start-over';
  startOverLine.append(startOver);
  const ask = element('p', 'explainer way-note', COPY.startOverAsk);
  ask.id = 'way-start-over-ask';
  const yes = linkButton(COPY.startOverYes);
  yes.id = 'btn-start-over-yes';
  const no = linkButton(COPY.startOverNo);
  no.id = 'btn-start-over-no';
  ask.append(' ', yes, no);
  ask.hidden = true;
  seeker.after(throwaway, throwawayNote, startOverLine, ask);

  // "Start over" shows only while a throwaway is stored and this browser can make one.
  const refresh = () => {
    ask.hidden = true;
    startOverLine.hidden = throwaway.hidden || !storedKey();
  };
  refresh();
  new MutationObserver(() => { if (!$('screen-signin').hidden) refresh(); })
    .observe($('screen-signin'), { attributes: true, attributeFilter: ['hidden'] });

  const ways = [seeker, throwaway, startOver, yes, no];
  const setWaysDisabled = on => { for (const b of ways) b.disabled = on; };
  new MutationObserver(() => { for (const b of ways.slice(1)) b.disabled = seeker.disabled; })
    .observe(seeker, { attributes: true, attributeFilter: ['disabled'] });
  const idle = () => !seeker.disabled;

  // A Seeker tap ends the automatic resume at once, even if MWA then fails; the key stays, so
  // "Just try it" still signs in as the same wallet.
  seeker.addEventListener('click', () => {
    if (!idle()) return;
    writeTab(WAY_KEY, 'seeker');
    removeKept(WAY_KEY);
  });

  const startThrowaway = async () => {
    setWaysDisabled(true); // a disabled button fires no click: no other sign-in starts now
    const key = await throwawayKey();
    setWaysDisabled(false);
    if (!key) {
      $('signin-status').textContent = COPY.throwawayUnsupported;
      throwaway.hidden = true;
      throwawayNote.hidden = true;
      refresh();
      return;
    }
    writeTab(WAY_KEY, 'throwaway');
    onSignIn(async () => {
      const token = await throwawaySignIn(key);
      writeKept(WAY_KEY, 'throwaway'); // remembered only once the server has signed this wallet in
      return token;
    }, {
      genesisNote: { title: COPY.genesisThrowawayTitle, body: COPY.genesisThrowawayBody },
    });
  };

  throwaway.addEventListener('click', () => { if (idle()) startThrowaway(); });
  startOver.addEventListener('click', () => {
    if (!idle()) return;
    startOverLine.hidden = true;
    ask.hidden = false;
  });
  no.addEventListener('click', () => { if (idle()) refresh(); });
  yes.addEventListener('click', () => {
    if (!idle()) return;
    removeKept(THROWAWAY_KEY);
    removeTab(THROWAWAY_KEY);
    removeKept(WAY_KEY);
    ask.hidden = true;
    startThrowaway(); // a fresh wallet, signed in as a tap on "Just try it" would be
  });

  // After a resume, the reveal offers the other way in until the player continues.
  const another = linkButton(COPY.signInAnother);
  another.id = 'btn-signin-another';
  another.hidden = true;
  $('btn-continue').after(another);
  new MutationObserver(() => { if (!$('screen-reveal').hidden) another.hidden = !resumed; })
    .observe($('screen-reveal'), { attributes: true, attributeFilter: ['hidden'] });
  $('btn-continue').addEventListener('click', () => { resumed = false; another.hidden = true; }, { capture: true });
  another.addEventListener('click', () => {
    resumed = false;
    another.hidden = true;
    removeKept(WAY_KEY); // the key stays: "Just try it" is still the same wallet
    store.clearAll();
    toSignIn('');
  });

  setWalkthroughWhen(() => wayIn() !== 'seeker');
  mountCard();
}
