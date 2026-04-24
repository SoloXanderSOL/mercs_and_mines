use std::sync::Arc;
use axum::{
    extract::{FromRef, FromRequestParts},
    http::{request::Parts, StatusCode},
    Json,
};
use chrono::Utc;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Value};
use uuid::Uuid;
use shared::SessionToken;
use crate::state::AppState;

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
        expires_at:     now + 7200, // 2-hour TEEPIN session window
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
