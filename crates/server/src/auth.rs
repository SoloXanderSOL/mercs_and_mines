use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use axum::{
    extract::{FromRef, FromRequestParts},
    http::{request::Parts, StatusCode},
    Json,
};
use chrono::Utc;
use dashmap::DashMap;
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use shared::SessionToken;
use crate::state::{AppState, PendingChallenge};
use crate::rate_limit::{ClientKey, RateWarn, SweepThrottle};

// ── Challenge keys and wallet validation ───────────────────────────────────

/// The pending-challenge map key: SHA-256 of the full challenge text. The client sends that text back verbatim, so verify finds its entry without trusting the wallet string.
pub fn challenge_key(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

/// A wallet address is bs58 of exactly 32 bytes that decode to a curve point that is not small-order. A small-order key can be signed for without its private key unless verification is strict (GAP-80); an off-curve address can never sign.
pub fn decode_wallet(addr: &str) -> Option<[u8; 32]> {
    let b: [u8; 32] = bs58::decode(addr).into_vec().ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&b).ok().filter(|k| !k.is_weak()).map(|_| b)
}

// ── Expiry sweeps ──────────────────────────────────────────────────────────

pub const AUTH_SWEEP_INTERVAL_SECS: u64 = 60;

/// Removes challenges past their expiry; keeps `expires_at >= now`, the boundary `verify`
/// still accepts. Returns the number removed.
pub fn sweep_expired_challenges(map: &DashMap<[u8; 32], PendingChallenge>, now: i64) -> usize {
    let mut removed = 0;
    map.retain(|_, v| {
        let keep = v.expires_at >= now;
        if !keep {
            removed += 1;
        }
        keep
    });
    removed
}

/// Removes sessions past their expiry; keeps `expires_at >= now`, the boundary
/// `AuthSession` still accepts. Returns the number removed.
pub fn sweep_expired_sessions(map: &DashMap<String, SessionToken>, now: i64) -> usize {
    let mut removed = 0;
    map.retain(|_, v| {
        let keep = v.expires_at >= now;
        if !keep {
            removed += 1;
        }
        keep
    });
    removed
}

// ── Bounded lists (ODQ-48) ─────────────────────────────────────────────────

/// Live sessions per `ClientKey` before a new one signs out that key's oldest.
pub const SESSION_QUOTA: usize = 10;
/// Live sessions in all before a sign-in that would evict nobody is refused (`503`).
pub const SESSION_CAP: usize = 100_000;
/// Pending challenges before a new one evicts an arbitrary entry.
pub const CHALLENGE_CAP: usize = 100_000;

/// A sign-in refused at the session backstop.
#[derive(Debug, PartialEq, Eq)]
pub struct SessionsFull;

/// The per-`ClientKey` session quota and the global backstop, kept beside `sessions`.
///
/// - Each key's deque holds `(token_id, expires_at)` in issue order. Entries that have
///   expired, or that the extractor or the sweep removed from `sessions`, are pruned lazily.
/// - `ClientKey::Shared` is never indexed: only the backstop applies to it.
/// - **Lock order is always this index, then `sessions`' shards.** Never take the index
///   while a `sessions` guard is alive. The `std` `Mutex` is never held across an `.await`.
pub struct SessionIndex {
    inner: Mutex<HashMap<ClientKey, VecDeque<(String, i64)>>>,
    quota: usize,
    cap: usize,
    full_warn: RateWarn,
}

impl SessionIndex {
    pub fn new(quota: usize, cap: usize) -> Self {
        Self { inner: Mutex::new(HashMap::new()), quota, cap, full_warn: RateWarn::new() }
    }

    /// Admits `token` for `key` in one decision under the index lock. A key at its quota
    /// evicts its oldest session. Otherwise, if `sessions` is at the backstop, the sign-in is
    /// refused and nothing is evicted, so a refusal never logs anyone out.
    pub fn admit(
        &self,
        sessions: &DashMap<String, SessionToken>,
        key: ClientKey,
        token: SessionToken,
        now: i64,
        at: Instant,
    ) -> Result<(), SessionsFull> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let live = match inner.get_mut(&key) {
            Some(q) => {
                q.retain(|(id, exp)| *exp >= now && sessions.contains_key(id));
                q.len()
            }
            None => 0,
        };
        let evicts = key != ClientKey::Shared && live >= self.quota;
        if !evicts && sessions.len() >= self.cap {
            if let Some(n) = self.full_warn.hit(at) {
                tracing::warn!(refused = n, cap = self.cap, "sign-in refused: the session map is at its backstop");
            }
            return Err(SessionsFull);
        }
        if key != ClientKey::Shared {
            let q = inner.entry(key).or_default();
            if evicts {
                if let Some((oldest, _)) = q.pop_front() {
                    sessions.remove(&oldest);
                }
            }
            q.push_back((token.token_id.clone(), token.expires_at));
        }
        sessions.insert(token.token_id.clone(), token);
        Ok(())
    }

    /// Drops index entries that have expired or are gone from `sessions`, and empty keys.
    /// The sweep task calls it after `sweep_expired_sessions`.
    pub fn prune(&self, sessions: &DashMap<String, SessionToken>, now: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.retain(|_, q| {
            q.retain(|(id, exp)| *exp >= now && sessions.contains_key(id));
            !q.is_empty()
        });
    }

    #[cfg(test)]
    pub(crate) fn indexed(&self) -> usize {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.values().map(VecDeque::len).sum()
    }
}

/// The pending-challenge cap: at the cap, sweep expired entries (at most once per
/// `SWEEP_MIN_INTERVAL`), then evict an arbitrary entry. Soft: two concurrent inserts at
/// the cap may each evict one. It is the only production writer of `pending_challenges`.
pub struct ChallengeCap {
    cap: usize,
    sweep: SweepThrottle,
    evict_warn: RateWarn,
}

impl ChallengeCap {
    pub fn new(cap: usize) -> Self {
        Self { cap, sweep: SweepThrottle::new(), evict_warn: RateWarn::new() }
    }

    /// Inserts `value` under `key`, making room first if the map is at the cap. No guard is
    /// alive at any `len`, `retain` or `remove` (the INF-6c-1 deadlock rule).
    pub fn insert(
        &self,
        map: &DashMap<[u8; 32], PendingChallenge>,
        key: [u8; 32],
        value: PendingChallenge,
        now: i64,
        at: Instant,
    ) {
        if !map.contains_key(&key) && map.len() >= self.cap {
            if self.sweep.allow(at) {
                sweep_expired_challenges(map, now);
            }
            if map.len() >= self.cap {
                let victim = map.iter().next().map(|e| *e.key());
                if let Some(k) = victim {
                    map.remove(&k);
                }
                if let Some(n) = self.evict_warn.hit(at) {
                    tracing::warn!(evicted = n, cap = self.cap, "pending-challenge map at its cap: evicting arbitrary entries");
                }
            }
        }
        map.insert(key, value);
    }
}

/// Sweeps both auth maps every `AUTH_SWEEP_INTERVAL_SECS`, then prunes the session index, so
/// entries nobody looks up again still leave memory. Run under `supervisor::supervise`.
pub async fn run_auth_sweep_task(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(AUTH_SWEEP_INTERVAL_SECS));
    loop {
        interval.tick().await;
        let now = Utc::now().timestamp();
        sweep_expired_challenges(&state.pending_challenges, now);
        sweep_expired_sessions(&state.sessions, now);
        state.session_index.prune(&state.sessions, now);
    }
}

// ── Challenge generation ───────────────────────────────────────────────────

/// Builds the human-readable challenge string the wallet must sign.
/// The format is intentionally verbose — Phantom displays it to the user
/// so they can see exactly what they are authorising.
pub fn build_challenge(wallet_address: &str, nonce: &str, timestamp: i64) -> String {
    format!(
        "Mercs & Mines — Session Authentication\n\
         \n\
         Wallet: {wallet_address}\n\
         Nonce:  {nonce}\n\
         Time:   {timestamp}\n\
         \n\
         Signing grants a 2-hour play session.\n\
         No funds will be transferred."
    )
}

// ── Signature verification ─────────────────────────────────────────────────

/// Verifies a base58-encoded Ed25519 signature over the challenge string.
/// Returns Ok(()) if valid, Err(message) if anything fails.
/// Strict RFC 8032 verification: small-order keys and small-order R are refused (GAP-80).
pub fn verify_wallet_signature(
    wallet_address: &str,
    challenge: &str,
    signature_b58: &str,
) -> Result<(), String> {
    let pubkey_bytes = bs58::decode(wallet_address)
        .into_vec()
        .map_err(|e| format!("invalid wallet address: {e}"))?;
    let pubkey_arr: [u8; 32] = pubkey_bytes
        .try_into()
        .map_err(|_| "wallet address must be 32 bytes".to_string())?;
    let verifying_key = VerifyingKey::from_bytes(&pubkey_arr)
        .map_err(|e| format!("invalid pubkey: {e}"))?;

    let sig_bytes = bs58::decode(signature_b58)
        .into_vec()
        .map_err(|e| format!("invalid signature encoding: {e}"))?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())?;
    let signature = Signature::from_bytes(&sig_arr);

    verifying_key
        .verify_strict(challenge.as_bytes(), &signature)
        .map_err(|e| format!("signature verification failed: {e}"))
}

// ── Token issuance ─────────────────────────────────────────────────────────

/// Builds a session for `wallet_address` and admits it through the session index (ODQ-48).
/// account_id = wallet_address for Phase 0 (no DB yet — Batch 8 replaces this).
pub fn admit_session(
    state: &AppState,
    wallet_address: &str,
    key: ClientKey,
) -> Result<SessionToken, SessionsFull> {
    let now = Utc::now().timestamp();
    let token = SessionToken {
        token_id:       Uuid::new_v4().to_string(),
        account_id:     wallet_address.to_string(), // Phase 0 stub — replace with DB lookup in Batch 8
        wallet_address: wallet_address.to_string(),
        issued_at:      now,
        expires_at:     now + state.config.server.session_duration_secs,
    };
    state.session_index.admit(&state.sessions, key, token.clone(), now, Instant::now())?;
    Ok(token)
}

// ── AuthSession extractor ──────────────────────────────────────────────────

/// Axum extractor for protected routes. Reads `Authorization: Bearer <token_id>`,
/// validates the token exists and has not expired, and returns the SessionToken.
/// Add this as a handler parameter to require authentication.
pub struct AuthSession(pub SessionToken);

#[axum::async_trait]
impl<S> FromRequestParts<S> for AuthSession
where
    S: Send + Sync,
    Arc<AppState>: axum::extract::FromRef<S>,
{
    type Rejection = (StatusCode, Json<Value>);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app: Arc<AppState> = Arc::from_ref(state);

        let token_id = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or_else(|| {
                (StatusCode::UNAUTHORIZED, Json(json!({"error": "missing Authorization: Bearer <token>"})))
            })?;

        let entry = app.sessions.get(token_id).ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, Json(json!({"error": "session not found or already expired"})))
        })?;

        let now = Utc::now().timestamp();
        if now > entry.expires_at {
            drop(entry);
            app.sessions.remove(token_id);
            return Err((StatusCode::UNAUTHORIZED, Json(json!({"error": "session expired — re-authenticate"}))));
        }

        Ok(AuthSession(entry.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    fn challenge(expires_at: i64) -> PendingChallenge {
        PendingChallenge {
            wallet_address: "w".into(),
            challenge_text: "c".into(),
            expires_at,
        }
    }

    fn session(id: &str, expires_at: i64) -> SessionToken {
        SessionToken {
            token_id: id.into(),
            account_id: "a".into(),
            wallet_address: "w".into(),
            issued_at: 0,
            expires_at,
        }
    }

    #[test]
    fn sweep_expired_challenges_keeps_live_entries() {
        let map = DashMap::new();
        map.insert([1u8; 32], challenge(NOW - 1));
        map.insert([2u8; 32], challenge(NOW));
        map.insert([3u8; 32], challenge(NOW + 60));
        assert_eq!(sweep_expired_challenges(&map, NOW), 1);
        assert!(!map.contains_key(&[1u8; 32]), "the expired challenge is gone");
        assert!(map.contains_key(&[2u8; 32]), "the boundary challenge is kept");
        assert!(map.contains_key(&[3u8; 32]), "the live challenge is kept");
    }

    fn identity_address() -> String {
        let mut b = [0u8; 32];
        b[0] = 1;
        bs58::encode(b).into_string()
    }

    /// GAP-80: weak keys (the all-zero and the identity point) and off-curve bytes are refused.
    /// `[2u8; 32]` is the smallest `[n; 32]` that is not a curve point.
    #[test]
    fn decode_wallet_refuses_weak_and_off_curve_keys() {
        assert_eq!(decode_wallet(&bs58::encode([0u8; 32]).into_string()), None, "all-zero is weak");
        assert_eq!(decode_wallet(&identity_address()), None, "the identity is weak");
        assert_eq!(decode_wallet(&bs58::encode([2u8; 32]).into_string()), None, "off-curve");
        assert_eq!(decode_wallet(&bs58::encode([9u8; 32]).into_string()), Some([9u8; 32]));
    }

    /// GAP-80: identity key, R = identity, s = 0 passes non-strict `verify` on every text.
    #[test]
    fn verify_wallet_signature_refuses_the_identity_forgery() {
        let mut sig = [0u8; 64];
        sig[0] = 1;
        let forged = bs58::encode(sig).into_string();
        assert!(verify_wallet_signature(&identity_address(), "any fixed text", &forged).is_err());
    }

    #[test]
    fn sweep_expired_sessions_keeps_live_entries() {
        let map = DashMap::new();
        for (id, exp) in [("expired", NOW - 1), ("boundary", NOW), ("live", NOW + 60)] {
            map.insert(id.to_string(), session(id, exp));
        }
        assert_eq!(sweep_expired_sessions(&map, NOW), 1);
        assert!(!map.contains_key("expired"), "the expired session is gone");
        assert!(map.contains_key("boundary"), "the boundary session is kept");
        assert!(map.contains_key("live"), "the live session is kept");
    }

    fn key(n: u8) -> ClientKey {
        ClientKey::V4(std::net::Ipv4Addr::new(192, 0, 2, n))
    }

    fn admit(ix: &SessionIndex, map: &DashMap<String, SessionToken>, k: ClientKey, id: &str) -> Result<(), SessionsFull> {
        ix.admit(map, k, session(id, NOW + 60), NOW, Instant::now())
    }

    #[test]
    fn session_index_evicts_the_oldest_at_the_quota() {
        let (ix, map) = (SessionIndex::new(2, 100), DashMap::new());
        for id in ["s1", "s2", "s3"] {
            assert_eq!(admit(&ix, &map, key(1), id), Ok(()));
        }
        assert!(!map.contains_key("s1"), "the oldest is signed out");
        assert!(map.contains_key("s2") && map.contains_key("s3"));
        assert_eq!(map.len(), 2);
        assert_eq!(ix.indexed(), 2);
    }

    #[test]
    fn session_index_refuses_at_the_backstop_and_evicts_nothing() {
        let (ix, map) = (SessionIndex::new(10, 3), DashMap::new());
        for (n, id) in [(1, "s1"), (2, "s2"), (3, "s3")] {
            assert_eq!(admit(&ix, &map, key(n), id), Ok(()));
        }
        assert_eq!(admit(&ix, &map, key(4), "s4"), Err(SessionsFull));
        assert_eq!(map.len(), 3, "a refusal logs nobody out");
        assert!(!map.contains_key("s4"));
        assert!(ix.full_warn.fired(), "the refusal is journalled");
    }

    #[test]
    fn session_index_admits_a_quota_full_key_at_the_backstop() {
        let (ix, map) = (SessionIndex::new(2, 2), DashMap::new());
        for id in ["s1", "s2"] {
            assert_eq!(admit(&ix, &map, key(1), id), Ok(()));
        }
        assert_eq!(admit(&ix, &map, key(1), "s3"), Ok(()), "evicting frees the slot, so no refusal");
        assert!(!map.contains_key("s1") && map.contains_key("s3"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn session_index_never_indexes_the_shared_key() {
        let (ix, map) = (SessionIndex::new(1, 100), DashMap::new());
        for id in ["s1", "s2", "s3"] {
            assert_eq!(admit(&ix, &map, ClientKey::Shared, id), Ok(()));
        }
        assert_eq!(map.len(), 3, "no quota under Shared");
        assert_eq!(ix.indexed(), 0);
    }

    /// The stale entry is the newer one, so a skipped prune would evict the live `s1` (N-n).
    #[test]
    fn session_index_prunes_stale_entries_lazily() {
        let (ix, map) = (SessionIndex::new(2, 100), DashMap::new());
        for id in ["s1", "s2"] {
            assert_eq!(admit(&ix, &map, key(1), id), Ok(()));
        }
        map.remove("s2").expect("admitted");
        assert_eq!(admit(&ix, &map, key(1), "s3"), Ok(()));
        assert!(map.contains_key("s1") && map.contains_key("s3"), "the stale entry made room; nothing was evicted");
        assert_eq!(ix.indexed(), 2);
    }

    #[test]
    fn challenge_cap_evicts_one_when_every_entry_is_live() {
        let (cap, map) = (ChallengeCap::new(2), DashMap::new());
        let t0 = Instant::now();
        for k in 1..=3u8 {
            cap.insert(&map, [k; 32], challenge(NOW + 60), NOW, t0);
        }
        assert_eq!(map.len(), 2, "one arbitrary entry was evicted");
        assert!(map.contains_key(&[3u8; 32]), "the new challenge is stored");
        assert!(cap.evict_warn.fired(), "the eviction is journalled");
    }

    #[test]
    fn challenge_cap_sweeps_expired_entries_before_evicting() {
        let (cap, map) = (ChallengeCap::new(2), DashMap::new());
        let t0 = Instant::now();
        cap.insert(&map, [1u8; 32], challenge(NOW - 1), NOW, t0);
        cap.insert(&map, [2u8; 32], challenge(NOW + 60), NOW, t0);
        cap.insert(&map, [3u8; 32], challenge(NOW + 60), NOW, t0);
        assert!(!map.contains_key(&[1u8; 32]), "the expired entry was swept");
        assert!(map.contains_key(&[2u8; 32]) && map.contains_key(&[3u8; 32]), "the live entries are kept");
        assert!(!cap.evict_warn.fired(), "the sweep freed a slot, so nothing was evicted");
    }

    /// Two clocks, both injected: `now` (i64) decides expiry, `at` (Instant) the throttle.
    #[test]
    fn challenge_cap_sweeps_at_most_once_a_second() {
        let (cap, map) = (ChallengeCap::new(3), DashMap::new());
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        for k in 1..=3u8 {
            cap.insert(&map, [k; 32], challenge(NOW + 10), NOW, t0);
        }
        cap.insert(&map, [4u8; 32], challenge(NOW + 1000), NOW, t0);
        assert_eq!(map.len(), 3, "the first sweep found nothing expired, so one entry was evicted");
        cap.insert(&map, [5u8; 32], challenge(NOW + 1000), NOW + 50, t0 + ms(500));
        assert_eq!(map.len(), 3, "throttled: no sweep, so the expired entries stay and one is evicted");
        cap.insert(&map, [6u8; 32], challenge(NOW + 1000), NOW + 50, t0 + ms(1500));
        assert!(map.contains_key(&[6u8; 32]));
        assert!(map.iter().all(|e| e.value().expires_at >= NOW + 50), "a second later the sweep runs again");
    }
}
