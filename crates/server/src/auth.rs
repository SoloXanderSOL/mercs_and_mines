use std::sync::Arc;
use std::time::Duration;
use axum::{
    extract::{FromRef, FromRequestParts},
    http::{request::Parts, StatusCode},
    Json,
};
use chrono::Utc;
use dashmap::DashMap;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use shared::SessionToken;
use crate::state::{AppState, PendingChallenge};

// ── Challenge keys and wallet validation ───────────────────────────────────

/// The pending-challenge map key: SHA-256 of the full challenge text. The client sends that text back verbatim, so verify finds its entry without trusting the wallet string.
pub fn challenge_key(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

/// A wallet address is bs58 of exactly 32 bytes. Not a curve-point check: a 32-byte non-point gets a challenge it can never verify.
pub fn decode_wallet(addr: &str) -> Option<[u8; 32]> {
    bs58::decode(addr).into_vec().ok()?.try_into().ok()
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

/// Sweeps both auth maps every `AUTH_SWEEP_INTERVAL_SECS`, so entries nobody looks up
/// again still leave memory. Run under `supervisor::supervise`.
pub async fn run_auth_sweep_task(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(AUTH_SWEEP_INTERVAL_SECS));
    loop {
        interval.tick().await;
        let now = Utc::now().timestamp();
        sweep_expired_challenges(&state.pending_challenges, now);
        sweep_expired_sessions(&state.sessions, now);
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
        .verify(challenge.as_bytes(), &signature)
        .map_err(|e| format!("signature verification failed: {e}"))
}

// ── Token issuance ─────────────────────────────────────────────────────────

/// Issues a new SessionToken and inserts it into the session store.
/// account_id = wallet_address for Phase 0 (no DB yet — Batch 8 replaces this).
pub fn issue_session(state: &Arc<AppState>, wallet_address: &str) -> SessionToken {
    let now = Utc::now().timestamp();
    let token = SessionToken {
        token_id:       Uuid::new_v4().to_string(),
        account_id:     wallet_address.to_string(), // Phase 0 stub — replace with DB lookup in Batch 8
        wallet_address: wallet_address.to_string(),
        issued_at:      now,
        expires_at:     now + state.config.server.session_duration_secs,
    };
    state.sessions.insert(token.token_id.clone(), token.clone());
    token
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
}
