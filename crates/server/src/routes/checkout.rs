use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::auth::AuthSession;
use crate::checkout_store::{Intent, IntentState, Settle};
use crate::payment::{Check, PayInstructions, Price, RailError, CHECKOUT_PRICE_USDC_MINOR};
use crate::rate_limit::RetryAfter;
use crate::state::AppState;

use super::{bad_gateway, client_ip, not_found, service_unavailable, too_many_requests, AppError};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/checkout/intent", post(post_checkout_intent))
        .route("/api/checkout/intent/:reference", get(get_checkout_intent))
}

const PRICE: Price = Price { amount_minor: CHECKOUT_PRICE_USDC_MINOR };

/// The client's view of an intent: what to pay, and where it stands. Amounts are strings
/// (a `u64` in JSON).
fn intent_view(intent: &Intent, now: Instant) -> Value {
    let PayInstructions::SolanaPay { reference, recipient, mint, amount_minor, decimals, cluster } = &intent.instructions;
    let mut view = json!({
        "reference": reference,
        "amount": amount_minor.to_string(),
        "decimals": decimals,
        "recipient": recipient,
        "mint": mint,
        "cluster": cluster,
        "expires_in_secs": intent.expires.saturating_duration_since(now).as_secs(),
    });
    let (status, extra) = match &intent.state {
        IntentState::Pending => ("pending", None),
        IntentState::Mismatch { paid } => ("mismatch", Some(("paid", json!(paid.to_string())))),
        IntentState::Confirmed(receipt) => ("confirmed", Some(("receipt", json!(receipt)))),
    };
    view["status"] = json!(status);
    if let Some((key, value)) = extra {
        view[key] = value;
    }
    view
}

/// A `503` the client retries after `Retry-After` seconds: the chain checks are over their
/// ceiling, or the RPC refused for rate.
fn check_busy(retry: RetryAfter) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, retry.0.to_string())],
        Json(json!({"error": "payment check busy, retry shortly"})),
    )
        .into_response()
}

/// Starts or resumes a checkout. `POST /api/checkout/intent` (BP-5, keeper), no body.
/// Order: `401` → `429` per address (`checkout_intent_ip`) → `429` per wallet
/// (`checkout_intent_wallet`) → `503` when no rail is configured → the wallet's unpaid
/// intent (`200`) or a new one (`201`).
/// - One unpaid intent per wallet, so a retry after a dropped wallet session never yields
///   a second reference to pay. A receipt or a mismatch is never reused: the next call
///   starts a new checkout.
/// - The price is the canon campaign price; the client sends nothing.
async fn post_checkout_intent(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ip = client_ip(ci, &headers, &state.rate_limits)?;
    let now = Instant::now();
    if let Err(r) = state.rate_limits.checkout_intent_ip.check(ip, now) {
        return Ok(too_many_requests(r));
    }
    if let Err(r) = state.rate_limits.checkout_intent_wallet.check(token.wallet_address.clone(), now) {
        return Ok(too_many_requests(r));
    }
    let Some(rail) = state.checkout_rail.as_ref() else {
        return Err(service_unavailable("checkout unavailable"));
    };
    let (intent, created) = state.checkout.current_or_create(&token.wallet_address, PRICE, now, || rail.instructions(PRICE));
    let status = if created { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, Json(intent_view(&intent, now))).into_response())
}

/// Where a checkout stands. `GET /api/checkout/intent/:reference` (BP-5, keeper).
/// Order: `401` → `429` per address (`checkout_confirm_ip`) → `429` per wallet
/// (`checkout_confirm_wallet`) → `404` (unknown, expired, or another wallet's: one body,
/// so it reveals nothing about other references) → a receipt or a mismatch, answered from
/// the store with no chain call → `503` when no rail is configured → `200 pending` when this
/// intent was checked in the last 2 s → `503` + `Retry-After` over the global ceiling
/// (`checkout_checks`, the bound on the RPC's per-address quota; journalled through
/// `RateWarn`) → the chain check → `200` pending, mismatch or confirmed; `503` when the RPC
/// refuses for rate; `502` with one `warn!` naming the cause (never the URL).
async fn get_checkout_intent(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    Path(reference): Path<String>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ip = client_ip(ci, &headers, &state.rate_limits)?;
    let now = Instant::now();
    if let Err(r) = state.rate_limits.checkout_confirm_ip.check(ip, now) {
        return Ok(too_many_requests(r));
    }
    if let Err(r) = state.rate_limits.checkout_confirm_wallet.check(token.wallet_address.clone(), now) {
        return Ok(too_many_requests(r));
    }
    let Some(intent) = state.checkout.get(&reference, now).filter(|i| i.wallet == token.wallet_address) else {
        return Err(not_found("unknown or expired payment"));
    };
    if intent.state != IntentState::Pending {
        return Ok(Json(intent_view(&intent, now)).into_response());
    }
    let Some(rail) = state.checkout_rail.as_ref() else {
        return Err(service_unavailable("checkout unavailable"));
    };
    if !state.checkout.begin_check(&reference, now) {
        return Ok(Json(intent_view(&intent, now)).into_response());
    }
    if let Err(r) = state.rate_limits.checkout_checks.check((), now) {
        if let Some(n) = state.rate_limits.checkout_checks_warn.hit(now) {
            tracing::warn!(refused = n, "checkout chain checks are over the global ceiling; answering 503");
        }
        return Ok(check_busy(r));
    }
    let settled = match rail.check(&intent.instructions, &token.wallet_address).await {
        Ok(Check::Pending) => return Ok(Json(intent_view(&intent, now)).into_response()),
        Ok(Check::Mismatch { paid, rail_tx }) => state.checkout.settle(&reference, IntentState::Mismatch { paid }, &rail_tx, now),
        Ok(Check::Paid(receipt)) => {
            let rail_tx = receipt.rail_tx.clone();
            state.checkout.settle(&reference, IntentState::Confirmed(receipt), &rail_tx, now)
        }
        Err(RailError::Busy) => return Ok(check_busy(RetryAfter(5))),
        Err(RailError::Upstream(e)) => {
            tracing::warn!("checkout check failed: {e}");
            return Err(bad_gateway("payment check failed"));
        }
    };
    match settled {
        Settle::Stored | Settle::SignatureUsed => {
            let latest = state.checkout.get(&reference, now).unwrap_or(intent);
            Ok(Json(intent_view(&latest, now)).into_response())
        }
        Settle::Gone => Err(not_found("unknown or expired payment")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::json_rpc::{JsonRpc, RpcUrl};
    use crate::routes::{serve_on_loopback, with_test_peer, TEST_PEER};
    use crate::solana_pay::fixtures::{exact, transfer, MINT, PRICE as PRICE_MINOR, RECIPIENT};
    use crate::solana_pay::SolanaPayRail;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;
    use tower::ServiceExt;

    /// In-memory sessions only (no DB): bytes 228–235 sit above BP-4's 221–227.
    const WALLET_A: [u8; 32] = [228u8; 32];
    const WALLET_B: [u8; 32] = [229u8; 32];
    const TEST_TIMEOUT: Duration = Duration::from_millis(250);

    // ── The mock RPC ──────────────────────────────────────────────────────────

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Normal,
        Http500,
        /// A JSON-RPC `error` with code 429, as the public devnet sends.
        RateLimited,
    }

    struct Mock {
        mode: Mode,
        /// Per reference: `(signature, transaction)`, oldest first.
        paid: Mutex<HashMap<String, Vec<(String, Value)>>>,
        calls: Mutex<Vec<Value>>,
    }

    impl Mock {
        fn new(mode: Mode) -> Arc<Self> {
            Arc::new(Self { mode, paid: Mutex::new(HashMap::new()), calls: Mutex::new(Vec::new()) })
        }

        fn pay(&self, reference: &str, signature: &str, tx: Value) {
            self.paid.lock().unwrap().entry(reference.to_string()).or_default().push((signature.to_string(), tx));
        }

        fn calls(&self) -> Vec<Value> {
            self.calls.lock().unwrap().clone()
        }
    }

    async fn mock_rpc(State(m): State<Arc<Mock>>, Json(req): Json<Value>) -> Response {
        m.calls.lock().unwrap().push(req.clone());
        match m.mode {
            Mode::Http500 => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            Mode::RateLimited => {
                return Json(json!({"jsonrpc": "2.0", "id": 1, "error": {"code": 429, "message": "Too many requests for a specific RPC call"}}))
                    .into_response();
            }
            Mode::Normal => {}
        }
        let key = req["params"][0].as_str().unwrap_or_default().to_string();
        let paid = m.paid.lock().unwrap();
        let result = match req["method"].as_str().unwrap_or_default() {
            "getSignaturesForAddress" => Value::Array(
                paid.get(&key)
                    .into_iter()
                    .flatten()
                    .rev()
                    .map(|(sig, _)| json!({"signature": sig, "slot": 1, "err": null, "blockTime": 1, "confirmationStatus": "finalized"}))
                    .collect(),
            ),
            "getTransaction" => {
                paid.values().flatten().find(|(sig, _)| *sig == key).map(|(_, tx)| tx.clone()).unwrap_or(Value::Null)
            }
            _ => return StatusCode::BAD_REQUEST.into_response(),
        };
        Json(json!({"jsonrpc": "2.0", "id": 1, "result": result})).into_response()
    }

    // ── State and requests ────────────────────────────────────────────────────

    fn sign_in(state: &AppState, token: &str, wallet: [u8; 32]) {
        let wallet = bs58::encode(wallet).into_string();
        let now = chrono::Utc::now().timestamp();
        state.sessions.insert(token.into(), shared::SessionToken {
            token_id: token.into(),
            account_id: wallet.clone(),
            wallet_address: wallet,
            issued_at: now,
            expires_at: now + 600,
        });
    }

    /// Sessions `"a"` (`WALLET_A`) and `"b"` (`WALLET_B`); with a mock, a rail pointed at it
    /// whose URL carries `api-key=SECRETKEY`, paying the fixtures' recipient in their mint.
    async fn state_with(mock: Option<&Arc<Mock>>) -> Arc<AppState> {
        let mut state = AppState::new(Arc::new(Config::default()));
        sign_in(&state, "a", WALLET_A);
        sign_in(&state, "b", WALLET_B);
        if let Some(m) = mock {
            let addr = serve_on_loopback(Router::new().route("/", post(mock_rpc)).with_state(Arc::clone(m))).await;
            let url = RpcUrl::parse(&format!("http://{addr}/?api-key=SECRETKEY")).expect("the mock URL parses");
            let rpc = JsonRpc::new(url, TEST_TIMEOUT, TEST_TIMEOUT).expect("the client builds");
            state.checkout_rail = Some(Arc::new(SolanaPayRail::new(rpc, RECIPIENT, MINT, "devnet")));
        }
        Arc::new(state)
    }

    async fn send(state: &Arc<AppState>, method: &str, uri: &str, token: Option<&str>) -> (StatusCode, HeaderMap, Value) {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let resp = with_test_peer(router().with_state(Arc::clone(state)), TEST_PEER)
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// Starts a checkout for `token`; returns its reference.
    async fn start(state: &Arc<AppState>, token: &str) -> String {
        let (status, _, body) = send(state, "POST", "/api/checkout/intent", Some(token)).await;
        assert!(status == StatusCode::CREATED || status == StatusCode::OK, "{status} {body}");
        body["reference"].as_str().expect("a reference").to_string()
    }

    async fn confirm(state: &Arc<AppState>, token: &str, reference: &str) -> (StatusCode, HeaderMap, Value) {
        send(state, "GET", &format!("/api/checkout/intent/{reference}"), Some(token)).await
    }

    /// A journal capture for one test (the `routes/mod.rs` recipe, held across awaits).
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn checkout_requires_a_session() {
        let mock = Mock::new(Mode::Normal);
        let state = state_with(Some(&mock)).await;
        assert_eq!(send(&state, "POST", "/api/checkout/intent", None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(send(&state, "GET", "/api/checkout/intent/RefAny", None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(mock.calls().len(), 0);
    }

    #[tokio::test]
    async fn checkout_without_a_rail_is_503() {
        let state = state_with(None).await;
        let (status, _, body) = send(&state, "POST", "/api/checkout/intent", Some("a")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body, json!({"error": "checkout unavailable"}));
    }

    #[tokio::test]
    async fn checkout_intent_is_reused_while_pending() {
        let mock = Mock::new(Mode::Normal);
        let state = state_with(Some(&mock)).await;
        let (s1, _, first) = send(&state, "POST", "/api/checkout/intent", Some("a")).await;
        let (s2, _, second) = send(&state, "POST", "/api/checkout/intent", Some("a")).await;
        assert_eq!((s1, s2), (StatusCode::CREATED, StatusCode::OK));
        assert_eq!(first["reference"], second["reference"]);
        assert_eq!(first["amount"], "3990000");
        assert_eq!(first["decimals"], 6);
        assert_eq!((first["recipient"].as_str(), first["mint"].as_str()), (Some(RECIPIENT), Some(MINT)));
        assert_eq!((first["cluster"].as_str(), first["status"].as_str()), (Some("devnet"), Some("pending")));
        assert_eq!(mock.calls().len(), 0, "starting a checkout makes no chain call");
    }

    #[tokio::test]
    async fn checkout_unpaid_intent_is_pending() {
        let mock = Mock::new(Mode::Normal);
        let state = state_with(Some(&mock)).await;
        let reference = start(&state, "a").await;
        let (status, _, body) = confirm(&state, "a", &reference).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "pending");
        let calls = mock.calls();
        assert_eq!(calls.len(), 1, "no signature, so no transaction to fetch");
        assert_eq!(calls[0]["method"], "getSignaturesForAddress");
        assert_eq!(calls[0]["params"], json!([reference, {"limit": 10, "commitment": "confirmed"}]));
    }

    #[tokio::test]
    async fn checkout_exact_payment_issues_a_receipt() {
        let mock = Mock::new(Mode::Normal);
        let state = state_with(Some(&mock)).await;
        let reference = start(&state, "a").await;
        mock.pay(&reference, "SigOne", exact(&reference));
        let (status, _, body) = confirm(&state, "a", &reference).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "confirmed");
        assert_eq!(body["receipt"]["intent_id"], reference.as_str());
        assert_eq!(body["receipt"]["rail_tx"], "SigOne");
        assert_eq!(body["receipt"]["amount_minor"], PRICE_MINOR);
        assert_eq!(body["receipt"]["payer_is_session_wallet"], false);
        let calls = mock.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1]["method"], "getTransaction");
        assert_eq!(calls[1]["params"], json!(["SigOne", {"encoding": "jsonParsed", "maxSupportedTransactionVersion": 0, "commitment": "confirmed"}]));
    }

    /// A confirmed checkout answers from the store: no second chain call.
    #[tokio::test]
    async fn checkout_confirmed_intent_makes_no_rpc_call() {
        let mock = Mock::new(Mode::Normal);
        let state = state_with(Some(&mock)).await;
        let reference = start(&state, "a").await;
        mock.pay(&reference, "SigOne", exact(&reference));
        assert_eq!(confirm(&state, "a", &reference).await.2["status"], "confirmed");
        let before = mock.calls().len();
        let (status, _, body) = confirm(&state, "a", &reference).await;
        assert_eq!((status, body["status"].as_str()), (StatusCode::OK, Some("confirmed")), "{body}");
        assert_eq!(mock.calls().len(), before);
    }

    /// One transaction carries both wallets' references: it confirms the first checkout
    /// only, and the second stays pending.
    #[tokio::test]
    async fn checkout_one_transaction_confirms_one_intent() {
        let mock = Mock::new(Mode::Normal);
        let state = state_with(Some(&mock)).await;
        let (ref_a, ref_b) = (start(&state, "a").await, start(&state, "b").await);
        let mut tx = exact(&ref_a);
        tx["transaction"]["message"]["accountKeys"]
            .as_array_mut()
            .unwrap()
            .push(json!({"pubkey": ref_b, "signer": false, "writable": false, "source": "transaction"}));
        mock.pay(&ref_a, "SigShared", tx.clone());
        mock.pay(&ref_b, "SigShared", tx);
        assert_eq!(confirm(&state, "a", &ref_a).await.2["status"], "confirmed");
        let (status, _, body) = confirm(&state, "b", &ref_b).await;
        assert_eq!((status, body["status"].as_str()), (StatusCode::OK, Some("pending")), "{body}");
    }

    /// A pending checkout asked again at once is answered without a second chain call.
    #[tokio::test]
    async fn checkout_pending_check_is_throttled() {
        let mock = Mock::new(Mode::Normal);
        let state = state_with(Some(&mock)).await;
        let reference = start(&state, "a").await;
        assert_eq!(confirm(&state, "a", &reference).await.2["status"], "pending");
        assert_eq!(confirm(&state, "a", &reference).await.2["status"], "pending");
        assert_eq!(mock.calls().len(), 1);
    }

    /// Underpaid by one unit: a terminal mismatch, answered from the store the second time.
    #[tokio::test]
    async fn checkout_mismatch_is_terminal() {
        let mock = Mock::new(Mode::Normal);
        let state = state_with(Some(&mock)).await;
        let reference = start(&state, "a").await;
        mock.pay(&reference, "SigShort", transfer(&reference, MINT, RECIPIENT, PRICE_MINOR - 1));
        let (status, _, body) = confirm(&state, "a", &reference).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["status"].as_str(), body["paid"].as_str()), (Some("mismatch"), Some("3989999")));
        let before = mock.calls().len();
        assert_eq!(confirm(&state, "a", &reference).await.2["status"], "mismatch");
        assert_eq!(mock.calls().len(), before);
    }

    #[tokio::test]
    async fn checkout_another_wallets_intent_is_404() {
        let mock = Mock::new(Mode::Normal);
        let state = state_with(Some(&mock)).await;
        let reference = start(&state, "a").await;
        for (token, r) in [("b", reference.as_str()), ("a", "RefUnknown")] {
            let (status, _, body) = confirm(&state, token, r).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(body, json!({"error": "unknown or expired payment"}));
        }
        assert_eq!(mock.calls().len(), 0);
    }

    /// After a receipt the same wallet can pay again: a new checkout, a new reference.
    #[tokio::test]
    async fn checkout_new_intent_after_a_receipt() {
        let mock = Mock::new(Mode::Normal);
        let state = state_with(Some(&mock)).await;
        let first = start(&state, "a").await;
        mock.pay(&first, "SigOne", exact(&first));
        assert_eq!(confirm(&state, "a", &first).await.2["status"], "confirmed");
        let (status, _, body) = send(&state, "POST", "/api/checkout/intent", Some("a")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_ne!(body["reference"].as_str(), Some(first.as_str()));
    }

    #[tokio::test]
    async fn checkout_rpc_rate_limit_is_503() {
        let mock = Mock::new(Mode::RateLimited);
        let state = state_with(Some(&mock)).await;
        let reference = start(&state, "a").await;
        let (status, headers, body) = confirm(&state, "a", &reference).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body, json!({"error": "payment check busy, retry shortly"}));
        assert!(headers.contains_key(header::RETRY_AFTER));
    }

    /// The journal names the cause and never the URL's key.
    #[tokio::test]
    async fn checkout_rpc_failure_is_502_and_journals_no_key() {
        let mock = Mock::new(Mode::Http500);
        let state = state_with(Some(&mock)).await;
        let reference = start(&state, "a").await;
        let buf = Buf::default();
        let w = buf.clone();
        let sub = tracing_subscriber::fmt().with_writer(move || w.clone()).with_ansi(false).finish();
        let guard = tracing::subscriber::set_default(sub);
        let (status, _, body) = confirm(&state, "a", &reference).await;
        drop(guard);
        let journal = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body, json!({"error": "payment check failed"}));
        assert!(journal.contains("500 Internal Server Error"), "the journal lost the cause: {journal}");
        assert!(!journal.contains("SECRETKEY"), "the journal leaked the key: {journal}");
    }

    /// Four chain checks already spent in this 10 s window (a literal, not the const): the
    /// next is `503` with `Retry-After` and never reaches the mock.
    #[tokio::test]
    async fn checkout_global_check_ceiling_is_503() {
        let mock = Mock::new(Mode::Normal);
        let state = state_with(Some(&mock)).await;
        let reference = start(&state, "a").await;
        let now = Instant::now();
        for _ in 0..4 {
            state.rate_limits.checkout_checks.check((), now).expect("under the ceiling");
        }
        let (status, headers, body) = confirm(&state, "a", &reference).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body, json!({"error": "payment check busy, retry shortly"}));
        assert!(headers.contains_key(header::RETRY_AFTER));
        assert_eq!(mock.calls().len(), 0);
    }

    /// No rail: the first five starts answer `503`, the sixth meets the wallet limit.
    #[tokio::test]
    async fn checkout_intent_per_wallet_limit() {
        let state = state_with(None).await;
        let mut statuses = Vec::new();
        for _ in 0..6 {
            statuses.push(send(&state, "POST", "/api/checkout/intent", Some("a")).await.0);
        }
        let mut expected = vec![StatusCode::SERVICE_UNAVAILABLE; 5];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected);
    }

    /// An unknown reference: thirty `404`s, then the wallet limit.
    #[tokio::test]
    async fn checkout_confirm_per_wallet_limit() {
        let state = state_with(None).await;
        let mut statuses = Vec::new();
        for _ in 0..31 {
            statuses.push(confirm(&state, "a", "RefUnknown").await.0);
        }
        let mut expected = vec![StatusCode::NOT_FOUND; 30];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected);
    }
}
