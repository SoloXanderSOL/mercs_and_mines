//! The checkout intents, in memory and bounded (BP-5, keeper). The `dev` port adds a
//! Postgres store beside this one, as every repository has an in-memory twin; the guard
//! that one transaction confirms one intent becomes a `UNIQUE` constraint there.
//!
//! One lock, never held across an `.await`: every method takes it, does map work, and
//! returns owned data.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::payment::{PayInstructions, Price, Receipt};
use crate::rate_limit::RateWarn;

/// How long an unpaid intent lives: room for a wallet retry after a dropped session.
pub const INTENT_TTL: Duration = Duration::from_secs(30 * 60);
/// How long a receipt or a mismatch is kept (a booth day).
pub const SETTLED_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// The shortest gap between two chain checks of one intent.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(2);
pub const CHECKOUT_INTENT_CAP: usize = 10_000;
pub const CHECKOUT_SETTLED_CAP: usize = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntentState {
    Pending,
    /// A payment of another amount arrived. Terminal.
    Mismatch { paid: u64 },
    Confirmed(Receipt),
}

#[derive(Clone, Debug)]
pub struct Intent {
    pub wallet: String,
    pub price: Price,
    pub instructions: PayInstructions,
    pub expires: Instant,
    pub state: IntentState,
}

/// What `settle` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Settle {
    Stored,
    /// The transaction already confirmed another intent; this one stays pending.
    SignatureUsed,
    /// The intent expired or was evicted meanwhile.
    Gone,
}

struct Pending {
    intent: Intent,
    last_check: Option<Instant>,
}

#[derive(Default)]
struct Inner {
    /// Unpaid intents by intent id.
    pending: HashMap<String, Pending>,
    /// Receipts and mismatches by intent id.
    settled: HashMap<String, Intent>,
    /// Each wallet's unpaid intent.
    current: HashMap<String, String>,
    /// Each receipt's transaction → its intent id: one transaction, one receipt.
    used: HashMap<String, String>,
}

pub struct CheckoutStore {
    inner: Mutex<Inner>,
    intent_cap: usize,
    settled_cap: usize,
    evict_warn: RateWarn,
}

impl Default for CheckoutStore {
    fn default() -> Self {
        Self::new(CHECKOUT_INTENT_CAP, CHECKOUT_SETTLED_CAP)
    }
}

impl CheckoutStore {
    pub fn new(intent_cap: usize, settled_cap: usize) -> Self {
        Self { inner: Mutex::new(Inner::default()), intent_cap, settled_cap, evict_warn: RateWarn::new() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The wallet's unpaid intent if it has a live one (`false`), else a new one from `make`
    /// (`true`). A receipt or a mismatch is never reused, so a wallet can pay again.
    pub fn current_or_create(&self, wallet: &str, price: Price, now: Instant, make: impl FnOnce() -> PayInstructions) -> (Intent, bool) {
        let mut inner = self.lock();
        if let Some(id) = inner.current.get(wallet).cloned() {
            match inner.pending.get(&id) {
                Some(p) if now < p.intent.expires => return (p.intent.clone(), false),
                Some(_) => {
                    inner.pending.remove(&id);
                }
                None => {}
            }
            inner.current.remove(wallet);
        }
        if inner.pending.len() >= self.intent_cap {
            inner.pending.retain(|_, p| now < p.intent.expires);
            if inner.pending.len() >= self.intent_cap {
                let victim = inner.pending.iter().min_by_key(|(_, p)| p.intent.expires).map(|(id, _)| id.clone());
                if let Some(id) = victim {
                    inner.pending.remove(&id);
                }
                if let Some(n) = self.evict_warn.hit(now) {
                    tracing::warn!(evicted = n, cap = self.intent_cap, "checkout intents at their cap: evicting the soonest to expire");
                }
            }
            let Inner { pending, current, .. } = &mut *inner;
            current.retain(|_, id| pending.contains_key(id));
        }
        let instructions = make();
        let id = instructions.intent_id().to_string();
        let intent = Intent { wallet: wallet.to_string(), price, instructions, expires: now + INTENT_TTL, state: IntentState::Pending };
        inner.pending.insert(id.clone(), Pending { intent: intent.clone(), last_check: None });
        inner.current.insert(wallet.to_string(), id);
        (intent, true)
    }

    /// The live intent with this id, pending or settled.
    pub fn get(&self, id: &str, now: Instant) -> Option<Intent> {
        let inner = self.lock();
        let intent = match inner.pending.get(id) {
            Some(p) => &p.intent,
            None => inner.settled.get(id)?,
        };
        (now < intent.expires).then(|| intent.clone())
    }

    /// True when a pending intent may be checked on chain now (none in the last
    /// `CHECK_INTERVAL`); it then stamps the check.
    pub fn begin_check(&self, id: &str, now: Instant) -> bool {
        let mut inner = self.lock();
        let Some(p) = inner.pending.get_mut(id) else { return false };
        if p.last_check.is_some_and(|t| now < t + CHECK_INTERVAL) {
            return false;
        }
        p.last_check = Some(now);
        true
    }

    /// Settles a pending intent as `Confirmed` or `Mismatch`, made by transaction `rail_tx`.
    /// A transaction that already confirmed another intent confirms nothing more.
    pub fn settle(&self, id: &str, state: IntentState, rail_tx: &str, now: Instant) -> Settle {
        let mut inner = self.lock();
        let confirms = matches!(state, IntentState::Confirmed(_));
        if confirms && inner.used.contains_key(rail_tx) {
            return Settle::SignatureUsed;
        }
        let Some(p) = inner.pending.remove(id) else { return Settle::Gone };
        if inner.current.get(&p.intent.wallet).map(String::as_str) == Some(id) {
            inner.current.remove(&p.intent.wallet);
        }
        if inner.settled.len() >= self.settled_cap {
            inner.settled.retain(|_, s| now < s.expires);
            if inner.settled.len() >= self.settled_cap {
                let victim = inner.settled.iter().min_by_key(|(_, s)| s.expires).map(|(id, _)| id.clone());
                if let Some(old) = victim {
                    inner.settled.remove(&old);
                }
                if let Some(n) = self.evict_warn.hit(now) {
                    tracing::warn!(evicted = n, cap = self.settled_cap, "checkout receipts at their cap: evicting the oldest");
                }
            }
            let Inner { settled, used, .. } = &mut *inner;
            used.retain(|_, id| settled.contains_key(id));
        }
        if confirms {
            inner.used.insert(rail_tx.to_string(), id.to_string());
        }
        let intent = Intent { expires: now + SETTLED_TTL, state, ..p.intent };
        inner.settled.insert(id.to_string(), intent);
        Settle::Stored
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckoutStore, IntentState, Settle, CHECK_INTERVAL, INTENT_TTL};
    use crate::payment::{PayInstructions, Price, Receipt};
    use std::time::{Duration, Instant};

    const PRICE: Price = Price { amount_minor: 3_990_000 };

    fn pay(id: &str) -> PayInstructions {
        PayInstructions::SolanaPay {
            reference: id.into(),
            recipient: "RecipientTest".into(),
            mint: "MintUsdcTest".into(),
            amount_minor: PRICE.amount_minor,
            decimals: 6,
            cluster: "devnet",
        }
    }

    fn receipt(id: &str, tx: &str) -> IntentState {
        IntentState::Confirmed(Receipt {
            intent_id: id.into(),
            rail_tx: tx.into(),
            payer: "PayerOne".into(),
            payer_is_session_wallet: true,
            amount_minor: PRICE.amount_minor,
            slot: 1,
            block_time: None,
            commitment: "confirmed".into(),
        })
    }

    #[test]
    fn store_reuses_a_live_intent_and_replaces_an_expired_one() {
        let store = CheckoutStore::default();
        let t0 = Instant::now();
        let (a, new_a) = store.current_or_create("w", PRICE, t0, || pay("RefA"));
        let (b, new_b) = store.current_or_create("w", PRICE, t0, || pay("RefB"));
        assert!(new_a && !new_b, "the second call reuses the first intent");
        assert_eq!(b.instructions.intent_id(), "RefA");
        assert_eq!(a.instructions, b.instructions);
        let later = t0 + INTENT_TTL + Duration::from_secs(1);
        assert!(store.get("RefA", later).is_none(), "an expired intent is gone");
        let (c, new_c) = store.current_or_create("w", PRICE, later, || pay("RefC"));
        assert!(new_c);
        assert_eq!(c.instructions.intent_id(), "RefC");
    }

    #[test]
    fn store_checks_at_most_every_two_seconds() {
        let store = CheckoutStore::default();
        let t0 = Instant::now();
        store.current_or_create("w", PRICE, t0, || pay("RefA"));
        assert!(store.begin_check("RefA", t0));
        assert!(!store.begin_check("RefA", t0 + Duration::from_millis(1999)));
        assert!(store.begin_check("RefA", t0 + CHECK_INTERVAL));
        assert!(!store.begin_check("RefUnknown", t0), "an unknown intent is never checked");
    }

    /// Three intents at a literal cap of 3: the fourth evicts the soonest to expire.
    #[test]
    fn store_evicts_the_soonest_expiring_intent_at_the_cap() {
        let store = CheckoutStore::new(3, 3);
        let t0 = Instant::now();
        for (i, id) in ["RefA", "RefB", "RefC"].into_iter().enumerate() {
            store.current_or_create(&format!("w{i}"), PRICE, t0 + Duration::from_secs(i as u64), || pay(id));
        }
        let t = t0 + Duration::from_secs(5);
        store.current_or_create("w3", PRICE, t, || pay("RefD"));
        assert!(store.get("RefA", t).is_none(), "the soonest to expire was evicted");
        for id in ["RefB", "RefC", "RefD"] {
            assert!(store.get(id, t).is_some(), "{id} is kept");
        }
        assert!(store.evict_warn.fired(), "the eviction is journalled");
    }

    /// One transaction confirms one intent; the second stays pending.
    #[test]
    fn store_one_transaction_confirms_one_intent() {
        let store = CheckoutStore::default();
        let t0 = Instant::now();
        store.current_or_create("w1", PRICE, t0, || pay("RefA"));
        store.current_or_create("w2", PRICE, t0, || pay("RefB"));
        assert_eq!(store.settle("RefA", receipt("RefA", "SigOne"), "SigOne", t0), Settle::Stored);
        assert_eq!(store.settle("RefB", receipt("RefB", "SigOne"), "SigOne", t0), Settle::SignatureUsed);
        assert_eq!(store.get("RefB", t0).map(|i| i.state), Some(IntentState::Pending));
    }

    /// A settled intent is not the wallet's current one: the next create mints a new intent.
    #[test]
    fn store_settled_intent_is_not_reused() {
        let store = CheckoutStore::default();
        let t0 = Instant::now();
        store.current_or_create("w", PRICE, t0, || pay("RefA"));
        assert_eq!(store.settle("RefA", receipt("RefA", "SigOne"), "SigOne", t0), Settle::Stored);
        let (next, created) = store.current_or_create("w", PRICE, t0, || pay("RefB"));
        assert!(created);
        assert_eq!(next.instructions.intent_id(), "RefB");
        assert!(matches!(store.get("RefA", t0).map(|i| i.state), Some(IntentState::Confirmed(_))));
    }
}
