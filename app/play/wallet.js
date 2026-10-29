// Sign-in through Mobile Wallet Adapter (BP-3a-1), the flow the device spike proved on the
// Seeker (Raw/DEVICE_SPIKE_RESULT.md): one `transact` that authorises, fetches a challenge for
// the authorised address, and signs it with `signMessages`. Seed Vault Wallet returns the bare
// 64-byte signature; other wallets return message + signature, so the last 64 bytes are taken
// either way. The checkout (BP-3c-2b) pays through the same wallet on the test network.

import { postChallenge, postVerify, ApiError } from './api.js';

const MWA_URL = 'https://cdn.jsdelivr.net/npm/@solana-mobile/mobile-wallet-adapter-protocol-web3js@3.0.1/+esm';
const WEB3_URL = 'https://cdn.jsdelivr.net/npm/@solana/web3.js@1.99.0/+esm';

// The wallet shows `name` and fetches `icon` relative to `uri`.
const IDENTITY = { name: 'Mercs & Mines', uri: location.origin, icon: 'assets/favicon-512.png' };

// The only persistent (cross-tab) storage in the client: the wallet's own authorisation, so a
// later sign-in skips the wallet's Connect sheet (spike Q1f). The server session stays per tab.
const AUTH_KEY = 'mm.mwa.auth_token';
const authKey = cluster => `${AUTH_KEY}.${cluster}`; // another chain's authorisation, kept apart
const MWA_AUTHORIZATION_FAILED = -1; // protocol error code for a stale or revoked auth_token

export class WalletError extends Error {
  constructor(cause) {
    super(cause && cause.message ? cause.message : String(cause));
    this.name = 'WalletError';
    this.code = cause ? cause.code : undefined;
  }
}

function readSaved(key = AUTH_KEY) {
  try {
    const raw = localStorage.getItem(key);
    const v = raw ? JSON.parse(raw) : null;
    return v && typeof v.auth_token === 'string' ? v : null;
  } catch { return null; }
}

function writeSaved(authToken, address, key = AUTH_KEY) {
  try { localStorage.setItem(key, JSON.stringify({ auth_token: authToken, address })); } catch { /* not kept */ }
}

function clearSaved(key = AUTH_KEY) {
  try { localStorage.removeItem(key); } catch { /* nothing to clear */ }
}

// ── encoding ────────────────────────────────────────────────────────────────
const B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

export function base58(bytes) {
  let n = 0n;
  for (const b of bytes) n = (n << 8n) + BigInt(b);
  let s = '';
  while (n > 0n) { s = B58[Number(n % 58n)] + s; n /= 58n; }
  for (const b of bytes) { if (b === 0) s = '1' + s; else break; }
  return s;
}

function base64ToBytes(s) {
  return Uint8Array.from(atob(s), c => c.charCodeAt(0));
}

// MWA accounts carry a base64 address; the server wants base58.
function addressOf(account) {
  const bytes = base64ToBytes(account.address);
  if (bytes.length !== 32) throw new WalletError(new Error('wallet returned an address that is not 32 bytes'));
  return { b64: account.address, b58: base58(bytes) };
}

// ── library ─────────────────────────────────────────────────────────────────
let loading = null;

// Imports both modules once. web3.js is the MWA wrapper's peer; importing the same URL keeps
// one instance for the checkout later.
export function loadWallet() {
  if (!loading) {
    loading = Promise.all([import(MWA_URL), import(WEB3_URL)]).then(([mwa]) => mwa);
    loading.catch(() => { loading = null; }); // a later tap may try the import again
  }
  return loading;
}

// ── sign-in ─────────────────────────────────────────────────────────────────
// Call only from a tap: `transact` opens the wallet through an intent. Resolves with the
// server's SessionToken. Throws ApiError (challenge or verify refused) or WalletError (the
// wallet failed, was dismissed, or is missing). Never retries: verify consumes the challenge.
export async function signIn() {
  const { transact } = await loadWallet();
  const saved = readSaved();
  let signed;
  try {
    signed = await transact(async wallet => {
      const auth = await wallet.authorize({
        chain: 'solana:mainnet',
        identity: IDENTITY,
        ...(saved ? { auth_token: saved.auth_token } : {}),
      });
      const addr = addressOf(auth.accounts[0]);
      writeSaved(auth.auth_token, addr.b58);
      // Fetched after authorize, inside the session, so the 60 s window starts as late as it can.
      const { challenge } = await postChallenge(addr.b58);
      const [payload] = await wallet.signMessages({
        addresses: [addr.b64],
        payloads: [new TextEncoder().encode(challenge)],
      });
      return { address: addr.b58, challenge, signature: base58(payload.slice(-64)) };
    });
  } catch (e) {
    if (e instanceof ApiError) throw e;
    if (saved && e && e.code === MWA_AUTHORIZATION_FAILED) clearSaved(); // the next tap starts clean
    throw new WalletError(e);
  }
  return postVerify(signed.address, signed.challenge, signed.signature);
}

// ── payment (BP-3c-2b) ──────────────────────────────────────────────────────
// The web3 module the checkout builds with: the same pinned import as at load, so no new fetch.
export function loadWeb3() {
  return loadWallet().then(() => import(WEB3_URL));
}

// Call only from a tap. One wallet session on `cluster`'s chain: authorise, then
// `prepare(address)` resolves with the payment for that account, or null to stop before anything
// is signed. Resolves with the signature (base58), or null. Throws ApiError or WalletError as
// signIn does. The authorisation is saved under the cluster's own key.
export async function payWithWallet(cluster, prepare) {
  const { transact } = await loadWallet();
  const key = authKey(cluster);
  const saved = readSaved(key);
  try {
    return await transact(async wallet => {
      const auth = await wallet.authorize({
        chain: `solana:${cluster}`,
        identity: IDENTITY,
        ...(saved ? { auth_token: saved.auth_token } : {}),
      });
      const addr = addressOf(auth.accounts[0]);
      writeSaved(auth.auth_token, addr.b58, key);
      const tx = await prepare(addr.b58);
      if (!tx) return null;
      const [signature] = await wallet.signAndSendTransactions({ transactions: [tx] });
      return signature;
    });
  } catch (e) {
    if (e instanceof ApiError) throw e;
    if (saved && e && e.code === MWA_AUTHORIZATION_FAILED) clearSaved(key);
    throw new WalletError(e);
  }
}
