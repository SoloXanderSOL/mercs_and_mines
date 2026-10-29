// The checkout (BP-3c-2b, demo plan §4a screen 8): "Buy your next campaign", $3.99 in test USDC
// on the Solana test network, paid through the wallet over Mobile Wallet Adapter.
//
// Everything the payment needs (recipient, mint, amount, decimals, reference, cluster) comes
// from the intent body: the client holds no address or amount of its own. One attempt:
//   1. a stored reference (this tab's last unfinished checkout) is checked first, so a retry
//      after a dropped wallet session never asks for a second payment; a 404 there falls through;
//   2. otherwise the intent POST (the server reuses an unpaid one), a simulation with the
//      session wallet as payer, then the wallet, and a poll until the payment is seen.
// A wallet that couldn't pay is never asked to sign. The reference and the receipt's ids never
// reach the DOM; the signature goes to the debug footer only and is never sent.

import { ApiError, store, postCheckoutIntent, getCheckoutIntent } from './api.js';
import { loadWeb3, payWithWallet, WalletError } from './wallet.js';

const TOKEN_PROGRAM = 'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA';
const ATA_PROGRAM = 'ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL';
const CLUSTER_RE = /^[a-z]+(-[a-z]+)?$/; // the cluster names a host below: nothing else passes
const POLL_MS = 3000;
const WINDOW_MS = 30000;
const NOT_SIGNED = [-1, -3]; // MWA: authorization failed, not signed: nothing went out

const nextPoll = ms => new Promise(r => setTimeout(r, ms)); // the checkout poll's only timer

// Booth copy, verbatim from the BP-3c-2b prompt.
const COPY = {
  checkoutTitle: 'Buy your next campaign',
  checkoutOfferBody: "A campaign runs up to 60 days and costs $3.99. Here it's paid in test USDC on the Solana test network: the payment is real, the money isn't.",
  checkoutPay: 'Pay $3.99 (test USDC)',
  checkoutNotNow: 'Not now',
  checkoutWorking: 'Drawing up the invoice...',
  checkoutWallet: 'Approve the payment in your wallet.',
  checkoutChecking: "Payment sent. Command's accountants are counting it...",
  checkoutPaidTitle: 'Paid. Your next campaign is booked.',
  checkoutPaidBody: '{amount} test USDC, paid on the Solana test network.',
  checkoutContinue: 'Continue',
  checkoutPendingBody: "The test network hasn't confirmed your payment yet. Nothing more will be charged. Check again in a moment.",
  checkoutCheckAgain: 'Check again',
  checkoutRetry: 'Try again',
  checkoutNoFunds: "This wallet has no test USDC, so it can't pay here. Nothing was sent.",
  checkoutNetwork: "Couldn't reach the Solana test network. Nothing was sent.",
  checkoutWrongWallet: 'Your wallet offered a different account from the one you signed in with. Nothing was sent.',
  checkoutRefused: "The wallet didn't finish the payment. Try again: we check first, so you can't pay twice.",
  checkoutSessionClosed: "The wallet closed before it said whether it paid. Try again: we check first, so you can't pay twice.",
  checkoutMismatch: "That payment didn't match the invoice. It's the test network, so no real money moved. Command has filed it under \"mysteries\".",
  checkoutLost: "Command lost track of this invoice. It's the test network, so no real money moved.",
  checkoutBusy: "Command's accountants are busy. Try again in {n} seconds.",
  checkoutUnavailable: "Checkout isn't open right now. Your campaign carries on regardless.",
  checkoutCheckFailed: "Couldn't check the payment just now. Nothing more will be charged.",
  walkthroughTitle: "This is where you'd pay.",
  walkthroughBody: "On a Solana Seeker, this screen takes $3.99 for your next campaign. At the booth, it's on us.",
};

const fill = (text, values) => text.replace(/\{(\w+)\}/g, (m, k) => (k in values ? String(values[k]) : m));

// "3990000", 6 → "3.99": BigInt only, the fraction trimmed of trailing zeros to two places at least.
export function amountText(amount, decimals) {
  const digits = String(BigInt(amount)).padStart(decimals + 1, '0');
  const whole = digits.slice(0, digits.length - decimals);
  let frac = digits.slice(digits.length - decimals).replace(/0+$/, '');
  while (frac.length < 2) frac += '0';
  return `${whole}.${frac}`;
}

// The way in can downgrade the real path to the walkthrough. By default it never does.
let walkthroughWhen = () => false;
export function setWalkthroughWhen(predicate) { walkthroughWhen = predicate; }

// ── the payment, built by hand over the pinned web3 (no token library) ─────────
function ataOf(w3, owner, mint) {
  const P = w3.PublicKey;
  return P.findProgramAddressSync([owner.toBytes(), new P(TOKEN_PROGRAM).toBytes(), mint.toBytes()], new P(ATA_PROGRAM))[0];
}

// The recipient's token account (created if missing, idempotent), then TransferChecked from the
// payer's, with the reference riding as an extra read-only key so the server can find it.
function buildPayment(w3, body, payerAddress, latest) {
  const P = w3.PublicKey;
  const payer = new P(payerAddress);
  const mint = new P(body.mint);
  const recipient = new P(body.recipient);
  const token = new P(TOKEN_PROGRAM);
  const from = ataOf(w3, payer, mint);
  const to = ataOf(w3, recipient, mint);
  const create = new w3.TransactionInstruction({
    programId: new P(ATA_PROGRAM),
    data: Uint8Array.from([1]), // CreateIdempotent
    keys: [
      { pubkey: payer, isSigner: true, isWritable: true },
      { pubkey: to, isSigner: false, isWritable: true },
      { pubkey: recipient, isSigner: false, isWritable: false },
      { pubkey: mint, isSigner: false, isWritable: false },
      { pubkey: w3.SystemProgram.programId, isSigner: false, isWritable: false },
      { pubkey: token, isSigner: false, isWritable: false },
    ],
  });
  const data = new Uint8Array(10); // TransferChecked: 12, the amount (u64 LE), the decimals
  data[0] = 12;
  new DataView(data.buffer).setBigUint64(1, BigInt(body.amount), true);
  data[9] = body.decimals;
  const transfer = new w3.TransactionInstruction({
    programId: token,
    data,
    keys: [
      { pubkey: from, isSigner: false, isWritable: true },
      { pubkey: mint, isSigner: false, isWritable: false },
      { pubkey: to, isSigner: false, isWritable: true },
      { pubkey: payer, isSigner: true, isWritable: false },
      { pubkey: new P(body.reference), isSigner: false, isWritable: false },
    ],
  });
  return new w3.Transaction({ feePayer: payer, blockhash: latest.blockhash, lastValidBlockHeight: latest.lastValidBlockHeight })
    .add(create, transfer);
}

// Resolves with the built payment, or null when the simulation fails (this wallet can't pay).
// Throws when the test network can't be reached.
async function simulated(w3, conn, body, payerAddress) {
  const latest = await conn.getLatestBlockhash('confirmed');
  const tx = buildPayment(w3, body, payerAddress, latest);
  const sim = await conn.simulateTransaction(tx);
  return sim.value.err === null ? tx : null;
}

// ── the screen ──────────────────────────────────────────────────────────────
// `deps` is main.js's side: $, show, handleError, note, rateText, offlineText, genericText and
// setSignature (the debug footer).
export function createCheckout(deps) {
  const { $ } = deps;
  let gen = 0;       // bumped whenever the screen is left: every await checks it
  let running = -1;  // the generation with a flow in flight (one tap at a time)
  let then = null;   // where Continue and Not now go

  const alive = g => g === gen && !$('screen-checkout').hidden;

  function render({ title = COPY.checkoutTitle, line = '', body = '', spinner = false, primary = null, secondary = null }) {
    $('checkout-title').textContent = title;
    $('checkout-line').textContent = line;
    $('checkout-line').hidden = !line;
    $('checkout-body').textContent = body;
    $('checkout-body').hidden = !body;
    $('checkout-spinner').hidden = !spinner;
    for (const [id, action] of [['btn-checkout-primary', primary], ['btn-checkout-secondary', secondary]]) {
      const button = $(id);
      button.hidden = !action;
      button.disabled = false;
      button.textContent = action ? action[0] : '';
      button.onclick = action ? action[1] : null;
    }
    deps.show('screen-checkout');
  }

  // Continue and Not now: the offer counts as made for this campaign once the player leaves.
  function leave(ref) {
    gen += 1;
    if (ref) store.clearCheckoutRef(ref);
    const camp = store.campaign();
    if (camp) store.setOffered(camp.campaign_id);
    const next = then;
    then = null;
    if (next) next();
  }

  const notNow = [COPY.checkoutNotNow, () => leave(false)];
  const cont = ref => [COPY.checkoutContinue, () => leave(ref)];
  const tryAgain = [COPY.checkoutRetry, () => start(attempt)];

  // The invoice and wallet steps can be left (Not now): the generation bumps, so a late answer does nothing.
  const working = (line, leavable = false) => render({ line, spinner: true, secondary: leavable ? notNow : null });
  const offerScreen = () => render({ body: COPY.checkoutOfferBody, primary: [COPY.checkoutPay, pay], secondary: notNow });
  const walkthrough = () => render({ title: COPY.walkthroughTitle, body: COPY.walkthroughBody, primary: cont(false) });
  const noFunds = () => render({ line: COPY.checkoutNoFunds, body: COPY.walkthroughBody, primary: cont(false) });
  const retryable = line => render({ line, primary: tryAgain, secondary: notNow });
  const unavailable = () => render({ body: COPY.checkoutUnavailable, primary: cont(false) });

  // One flow per tap: a second tap while one runs does nothing.
  async function start(flow) {
    if (running === gen) return;
    const g = gen;
    running = g;
    try {
      await flow(g);
    } catch (e) {
      if (alive(g)) failed(e);
    } finally {
      if (running === g) running = -1;
    }
  }

  function failed(e) {
    deps.note(e);
    if (e instanceof WalletError) {
      const unknown = e.code === 'ERROR_SESSION_CLOSED' || e.code === -4; // it may have paid: the retry checks first
      return retryable(unknown ? COPY.checkoutSessionClosed : COPY.checkoutRefused);
    }
    if (e instanceof ApiError) {
      if (e.kind === 'unauth') { gen += 1; return deps.handleError(e, null); }
      if (e.kind === 'rate') return retryable(deps.rateText(e.retryAfter));
      if (e.kind === 'offline') return retryable(deps.offlineText);
      if (e.kind === 'unavailable') return e.retryAfter ? retryable(fill(COPY.checkoutBusy, { n: e.retryAfter })) : unavailable();
      if (e.status === 502) return checkFailed();
      return retryable(deps.genericText);
    }
    return retryable(COPY.checkoutNetwork); // the web3 module or the test network failed
  }

  function checkFailed(reference = store.checkoutRef()?.reference) {
    render({
      body: COPY.checkoutCheckFailed,
      primary: reference ? [COPY.checkoutCheckAgain, () => start(g => poll(g, reference))] : tryAgain,
      secondary: cont(false),
    });
  }

  function settle(g, body) {
    if (body.status === 'confirmed') {
      return render({
        title: COPY.checkoutPaidTitle,
        body: fill(COPY.checkoutPaidBody, { amount: amountText(body.amount, body.decimals) }),
        primary: cont(body.reference),
      });
    }
    if (body.status === 'mismatch') return render({ body: COPY.checkoutMismatch, primary: cont(body.reference) });
    if (body.status === 'pending') return poll(g, body.reference);
    return checkFailed(body.reference); // a status this client doesn't know: never a re-GET by itself
  }

  // The first GET at once, then every 3 s (longer when a 503 says so) for at most 30 s.
  async function poll(g, reference) {
    working(COPY.checkoutChecking);
    const deadline = performance.now() + WINDOW_MS;
    let delay = 0;
    for (;;) {
      if (delay) {
        if (performance.now() + delay > deadline) {
          return render({
            body: COPY.checkoutPendingBody,
            primary: [COPY.checkoutCheckAgain, () => start(g2 => poll(g2, reference))],
            secondary: cont(false),
          });
        }
        await nextPoll(delay);
        if (!alive(g)) return;
      }
      let body;
      try {
        body = await getCheckoutIntent(reference);
      } catch (e) {
        if (!alive(g)) return;
        if (e instanceof ApiError && e.kind === 'notfound') {
          return render({ body: COPY.checkoutLost, primary: cont(reference) }); // never a fresh POST here
        }
        if (e instanceof ApiError && e.kind === 'unavailable' && e.retryAfter) {
          delay = Math.max(POLL_MS, e.retryAfter * 1000);
          continue;
        }
        throw e;
      }
      if (!alive(g)) return;
      if (body.status !== 'pending') return settle(g, body);
      delay = POLL_MS;
    }
  }

  // Pay and Try again.
  async function attempt(g) {
    working(COPY.checkoutWorking, true);
    const session = store.session();
    const wallet = (session && session.wallet_address) || null;

    // 1. This tab's unfinished checkout first: a confirmed one is never paid again.
    const stored = store.checkoutRef();
    if (stored && (!wallet || stored.wallet === wallet)) {
      let body = null;
      try {
        body = await getCheckoutIntent(stored.reference);
      } catch (e) {
        if (!(e instanceof ApiError && e.kind === 'notfound')) throw e;
        store.clearCheckoutRef(stored.reference); // expired, a restart, or not this wallet's
      }
      if (!alive(g)) return;
      if (body) return settle(g, body);
    }

    // 2. A fresh attempt.
    const body = await postCheckoutIntent();
    if (!alive(g)) return;
    if (!CLUSTER_RE.test(String(body.cluster))) return unavailable();
    const w3 = await loadWeb3();
    const conn = new w3.Connection(`https://api.${body.cluster}.solana.com`, 'confirmed');
    if (!alive(g)) return;

    if (wallet) { // (i) before the wallet opens: a wallet that can't pay never sees the payment
      let tx;
      try {
        tx = await simulated(w3, conn, body, wallet);
      } catch (e) {
        deps.note(e);
        return alive(g) && retryable(COPY.checkoutNetwork);
      }
      if (!alive(g)) return;
      if (!tx) return noFunds();
    }

    working(COPY.checkoutWallet, true);
    let stop = null;   // why prepare declined
    let handed = false;
    let signature;
    try {
      signature = await payWithWallet(body.cluster, async address => {
        // (ii) the account the wallet offers, simulated again on a fresh blockhash
        if (!alive(g)) return null; // Not now was tapped while the wallet opened
        if (wallet && address !== wallet) { stop = 'wrongWallet'; return null; }
        let tx;
        try {
          tx = await simulated(w3, conn, body, address);
        } catch (e) {
          deps.note(e);
          stop = 'network';
          return null;
        }
        if (!tx) { stop = 'noFunds'; return null; }
        if (!alive(g)) return null;
        store.setCheckoutRef({ reference: body.reference, wallet }); // stored at the handover: nothing before it went out
        handed = true;
        return tx;
      });
    } catch (e) {
      if (handed && e instanceof WalletError && NOT_SIGNED.includes(e.code)) store.clearCheckoutRef(body.reference);
      throw e;
    }
    if (!signature) {
      if (!alive(g)) return;
      if (stop === 'wrongWallet') return retryable(COPY.checkoutWrongWallet);
      if (stop === 'network') return retryable(COPY.checkoutNetwork);
      return noFunds();
    }
    deps.setSignature(signature);
    if (!alive(g)) return;
    return poll(g, body.reference);
  }

  function pay() {
    if (walkthroughWhen()) return walkthrough(); // copy only: no server call, no wallet
    return start(attempt);
  }

  // The entry: `next` runs when the player leaves (Continue or Not now).
  function offer(next) {
    gen += 1;
    then = next;
    offerScreen();
  }

  function fillCopy() {
    $('checkout-title').textContent = COPY.checkoutTitle;
  }

  fillCopy();
  return { offer };
}
