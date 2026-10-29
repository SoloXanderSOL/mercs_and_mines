// Three ways in for the Breakpoint demo (ODQ-69, demo-only: never merged to dev, where canon
// stays Seeker-only and MWA-only).
//
// 1. "Sign in on your Solana Seeker" and 2. "Sign in with your Solana wallet" both run today's
//    MWA sign-in unchanged. Nothing `authorize` returns names the wallet, so the button tapped
//    is the only thing that tells them apart: it is recorded as the way in.
// 3. "Just try it": a throwaway Ed25519 wallet made with WebCrypto in this tab. It signs the
//    server's challenge itself through the same two sign-in posts, so the server is unchanged.
//
// Kept in sessionStorage (this tab only, never the cross-tab store): the way in (mm.way_in) and the
// throwaway's private JWK (mm.throwaway.key). store.clearAll() never touches either, so a 401
// keeps the key and the next sign-in is the same wallet (the join answers AlreadyMember).

import { postChallenge, postVerify } from './api.js';
import { base58 } from './wallet.js';

const WAY_KEY = 'mm.way_in';
const THROWAWAY_KEY = 'mm.throwaway.key';
const WAYS = ['seeker', 'wallet', 'throwaway'];

// Booth copy, verbatim from the BP-3c-2 prompt.
const COPY = {
  waySeeker: 'Sign in on your Solana Seeker',
  wayExplainer: 'Your wallet will ask you to authorize. That only proves the wallet is yours: nothing is spent, nothing moves.',
  wayWallet: 'Sign in with your Solana wallet',
  wayWalletNote: '(Phantom and Solflare accepted)',
  wayThrowaway: 'Just try it (no wallet needed)',
  wayThrowawayNote: "We'll make a throwaway wallet in this tab. It holds nothing, and it's gone when you close the tab.",
  throwawayUnsupported: "This browser can't make a throwaway wallet. Try Chrome, or Safari on iOS 17 or later.",
  genesisThrowawayTitle: 'A wallet made for the occasion.',
  genesisThrowawayBody: 'It holds no Genesis Token, no funds and no grudges. Close this tab and it goes, along with your Commander.',
};

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

// The way this tab signed in, for BP-3c-2b's checkout. Absent or unknown reads as 'seeker'.
export function wayIn() {
  const v = readTab(WAY_KEY);
  return WAYS.includes(v) ? v : 'seeker';
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
  writeTab(THROWAWAY_KEY, JSON.stringify(jwk));
  return { privateKey: pair.privateKey, address: base58(pub) };
}

async function getOrCreate() {
  const raw = readTab(THROWAWAY_KEY);
  return raw ? importStored(raw) : generate();
}

// Resolves with {privateKey, address}, or null when this browser can't make one. A failure
// (no WebCrypto, no Ed25519, a corrupt stored key) removes the stored key and tries once more.
async function throwawayKey() {
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      if (!globalThis.crypto || !crypto.subtle) return null;
      return await getOrCreate();
    } catch {
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

// ── the sign-in screen ──────────────────────────────────────────────────────
function element(tag, className, text) {
  const node = document.createElement(tag);
  node.className = className;
  node.textContent = text;
  return node;
}

// Relabels the Seeker button and the explainer, and adds the other two ways after it. Call it
// after main.js has filled its copy and before it wires its own handlers, so the way is
// recorded before the Seeker button's sign-in starts. `btn-signin` is disabled while any
// sign-in runs; no way is recorded then.
export function mountWaysIn({ $, onSignIn }) {
  const seeker = $('btn-signin');
  seeker.textContent = COPY.waySeeker;
  $('signin-explainer').textContent = COPY.wayExplainer;

  const wallet = element('button', 'btn', COPY.wayWallet);
  wallet.type = 'button';
  wallet.id = 'btn-signin-wallet';
  const walletNote = element('p', 'explainer way-note', COPY.wayWalletNote);
  const throwaway = element('button', 'btn', COPY.wayThrowaway);
  throwaway.type = 'button';
  throwaway.id = 'btn-signin-throwaway';
  const throwawayNote = element('p', 'explainer way-note', COPY.wayThrowawayNote);
  seeker.after(wallet, walletNote, throwaway, throwawayNote);

  let preparing = false;
  const idle = () => !seeker.disabled && !preparing;

  seeker.addEventListener('click', () => { if (idle()) writeTab(WAY_KEY, 'seeker'); });
  wallet.addEventListener('click', () => {
    if (!idle()) return;
    writeTab(WAY_KEY, 'wallet');
    onSignIn();
  });
  throwaway.addEventListener('click', async () => {
    if (!idle()) return;
    preparing = true;
    const key = await throwawayKey();
    preparing = false;
    if (!key) {
      $('signin-status').textContent = COPY.throwawayUnsupported;
      throwaway.hidden = true;
      throwawayNote.hidden = true;
      return;
    }
    writeTab(WAY_KEY, 'throwaway');
    onSignIn(() => throwawaySignIn(key), {
      genesisNote: { title: COPY.genesisThrowawayTitle, body: COPY.genesisThrowawayBody },
    });
  });
}
