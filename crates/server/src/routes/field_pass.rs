//! The Breakpoint Genesis Field Pass routes (BP-7a, demo-only). The mint itself is
//! `crate::demo_field_pass`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::json;

use crate::auth::AuthSession;
use crate::demo_field_pass::{view, Attempt, ChainState, MintError, Minter};
use crate::json_rpc::RpcError;
use crate::rate_limit::{RetryAfter, GENESIS_ALL_PER_MINUTE};
use crate::state::AppState;

use super::{bad_gateway, client_ip, conflict, forbidden, internal_error, not_found, service_unavailable, too_many_requests, AppError};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/demo/field-pass", post(post_field_pass).get(get_field_pass))
}

/// A `503` the client retries after `Retry-After` seconds: chain work is over its ceiling,
/// or the devnet RPC refused for rate.
fn chain_busy(retry: RetryAfter) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, retry.0.to_string())],
        Json(json!({"error": "the pass desk is busy, retry shortly"})),
    )
        .into_response()
}

/// A mint step's failure as a response, with one journal line naming the cause.
fn mint_error(e: MintError) -> Response {
    match e {
        MintError::WrongCluster => service_unavailable("minting not open").into_response(),
        MintError::Rpc(RpcError::RateLimited(t)) => {
            tracing::warn!("field pass: the devnet RPC refused for rate: {t}");
            chain_busy(RetryAfter(10))
        }
        MintError::Rpc(RpcError::Failed(t)) | MintError::Reply(t) => {
            tracing::warn!("field pass: devnet RPC failed: {t}");
            bad_gateway("devnet RPC failed").into_response()
        }
    }
}

/// Counts one unit of chain work against the global ceiling.
fn chain_slot(state: &AppState, now: Instant) -> Result<(), Response> {
    state.field_pass.limits.chain.check((), now).map_err(|r| {
        if let Some(n) = state.field_pass.limits.chain_warn.hit(now) {
            tracing::warn!(refused = n, "field pass chain work is over its ceiling; answering 503");
        }
        chain_busy(r)
    })
}

/// Claims the signed-in wallet's Field Pass. `POST /api/demo/field-pass` (BP-7a, demo-only),
/// no body.
/// Order: `401` → `429` per address (`field_pass_ip`) → `429` per wallet (`field_pass_wallet`)
/// → `503` "minting not open" with no minter or no Genesis lookup → `503` + `Retry-After`
/// over the chain ceiling (`field_pass_chain`) → `503` over the Genesis ceiling
/// (`genesis_all`, never `genesis_wallet`: the sign-in already spent one) → the server's own
/// Genesis lookup → `502` / `403` no Genesis Token → the token's claim:
/// - **another wallet's: `409`**, with nothing of that claim in the body;
/// - **this wallet's: its chain state (`200`)**; a dead one is rebuilt below;
/// - **none, or dead:** the devnet fence → a fresh blockhash → build and sign in memory →
///   **one write**, `SET NX` (or the compare-and-set on the dead signature) → the send in a
///   spawned task → `201`. A lost write answers the winner's claim (`200`) or `409`.
///
/// Every `await` before the write writes nothing, so a dropped handler loses nothing. After
/// it, the stored attempt is the reference: the next read resends it or proves it dead.
async fn post_field_pass(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ip = client_ip(ci, &headers, &state.rate_limits)?;
    let now = Instant::now();
    let wallet = token.wallet_address.clone();
    if let Err(r) = state.field_pass.limits.ip.check(ip, now) {
        return Ok(too_many_requests(r));
    }
    if let Err(r) = state.field_pass.limits.wallet.check(wallet.clone(), now) {
        return Ok(too_many_requests(r));
    }
    let (Some(minter), Some(genesis)) = (state.field_pass.minter.clone(), state.genesis.as_ref()) else {
        return Err(service_unavailable("minting not open"));
    };
    if let Err(resp) = chain_slot(&state, now) {
        return Ok(resp);
    }
    if state.rate_limits.genesis_all.check((), now).is_err() {
        if let Some(n) = state.rate_limits.genesis_all_warn.hit(now) {
            tracing::warn!(refused = n, "genesis lookups are over the global ceiling ({GENESIS_ALL_PER_MINUTE}/min); answering 503");
        }
        return Err(service_unavailable("genesis lookup unavailable"));
    }
    let mint = match genesis.lookup(&wallet).await {
        Ok(Some(m)) => m,
        Ok(None) => return Err(forbidden("no Genesis Token")),
        Err(e) => {
            tracing::warn!("field pass: genesis lookup failed: {e}");
            return Err(bad_gateway("genesis lookup failed"));
        }
    };
    let claims = Arc::clone(&state.field_pass.claims);
    let stored = claims.get(&mint).await.map_err(|e| internal_error(format!("field pass claim read: {e}")))?;
    let dead = match stored {
        Some(a) if a.wallet != wallet => return Err(conflict("this Genesis Token's pass is already claimed")),
        Some(a) => {
            if let Err(e) = claims.link(&wallet, &mint).await {
                tracing::warn!("field pass link: {e}");
            }
            match minter.chain_state(&a).await {
                Ok(ChainState::Dead) => Some(a),
                Ok(s) => return Ok(Json(view(&a, s)).into_response()),
                Err(e) => return Ok(mint_error(e)),
            }
        }
        None => None,
    };
    if let Err(e) = minter.fence().await {
        return Ok(mint_error(e));
    }
    let (blockhash, last) = match minter.latest_blockhash().await {
        Ok(b) => b,
        Err(e) => return Ok(mint_error(e)),
    };
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).expect("OS RNG unavailable");
    let attempt = minter.build(&wallet, blockhash, last, seed).map_err(build_refused)?;
    let won = match &dead {
        None => claims.claim(&mint, &attempt).await,
        Some(old) => claims.replace(&mint, &old.signature, &attempt).await,
    }
    .map_err(|e| internal_error(format!("field pass claim write: {e}")))?;
    if !won {
        return lost_write(&*claims, &mint, &wallet).await;
    }
    if let Err(e) = claims.link(&wallet, &mint).await {
        tracing::warn!("field pass link: {e}");
    }
    spawn_send(minter, attempt.wire.clone());
    Ok((StatusCode::CREATED, Json(view(&attempt, ChainState::Pending))).into_response())
}

/// An owner the builder refuses (it equals the payer or a program): never a real wallet.
fn build_refused(e: impl Into<String>) -> AppError {
    tracing::warn!("field pass build refused: {}", e.into());
    forbidden("this wallet can't hold the pass")
}

/// Another request's write won: answer its claim, or `409` if another wallet made it.
async fn lost_write(claims: &dyn crate::demo_field_pass::Claims, mint: &str, wallet: &str) -> Result<Response, AppError> {
    match claims.get(mint).await.map_err(|e| internal_error(format!("field pass claim read: {e}")))? {
        Some(a) if a.wallet == wallet => Ok(Json(view(&a, ChainState::Pending)).into_response()),
        Some(_) => Err(conflict("this Genesis Token's pass is already claimed")),
        None => Err(internal_error("field pass claim lost after a refused write")),
    }
}

/// The first send, in its own task, so a dropped handler can't skip it.
fn spawn_send(minter: Arc<Minter>, wire: String) {
    tokio::spawn(async move {
        if let Err(e) = minter.send(&wire).await {
            tracing::info!("field pass send: {e} (the next status read resends)");
        }
    });
}

/// Where the signed-in wallet's pass stands. `GET /api/demo/field-pass` (BP-7a, demo-only).
/// Order: `401` → `429` per address (`field_pass_read_ip`) → `429` per wallet
/// (`field_pass_read_wallet`) → `503` "minting not open" with no minter → `404` (no claim
/// linked to this wallet, or the token's claim is now another wallet's) → `503` +
/// `Retry-After` over the chain ceiling → the chain state (`200` pending, confirmed or
/// dead; unseen with its blockhash alive, the stored bytes are resent).
async fn get_field_pass(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ip = client_ip(ci, &headers, &state.rate_limits)?;
    let now = Instant::now();
    let wallet = token.wallet_address.clone();
    if let Err(r) = state.field_pass.limits.read_ip.check(ip, now) {
        return Ok(too_many_requests(r));
    }
    if let Err(r) = state.field_pass.limits.read_wallet.check(wallet.clone(), now) {
        return Ok(too_many_requests(r));
    }
    let Some(minter) = state.field_pass.minter.clone() else {
        return Err(service_unavailable("minting not open"));
    };
    let claims = Arc::clone(&state.field_pass.claims);
    let read = |e: String| internal_error(format!("field pass claim read: {e}"));
    let Some(mint) = claims.mint_of(&wallet).await.map_err(read)? else {
        return Err(not_found("no field pass claim"));
    };
    let attempt: Attempt = match claims.get(&mint).await.map_err(read)? {
        Some(a) if a.wallet == wallet => a,
        _ => return Err(not_found("no field pass claim")),
    };
    if let Err(resp) = chain_slot(&state, now) {
        return Ok(resp);
    }
    match minter.chain_state(&attempt).await {
        Ok(s) => Ok(Json(view(&attempt, s)).into_response()),
        Err(e) => Ok(mint_error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::demo_field_pass::{Claims, InMemoryClaims, PauseHook, FIELD_PASS_NAME};
    use crate::genesis::fixtures::sgt;
    use crate::genesis::{GenesisRpc, TOKEN_2022_PROGRAM};
    use crate::json_rpc::RpcUrl;
    use crate::routes::{serve_on_loopback, with_test_peer, TEST_PEER};
    use axum::body::Body;
    use axum::http::Request;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;
    use tower::ServiceExt;

    /// In-memory sessions only: bytes 241 and 242, private to these tests.
    const HOLDER: [u8; 32] = [241u8; 32];
    const OTHER: [u8; 32] = [242u8; 32];
    /// Both clients' timeouts. The slow send waits 1,000 ms.
    const TEST_TIMEOUT: Duration = Duration::from_millis(250);
    /// The mock's SGT mint and its blockhash's last valid height.
    const MINT: &str = "MintSgtOne";
    const LAST_VALID: u64 = 1000;

    // ── The mock RPC: mainnet's lookup and devnet's mint on one URL ───────────

    #[derive(Clone, Copy, PartialEq)]
    enum Status {
        /// `null`, as an unknown or dropped signature reads.
        Unseen,
        Processed,
        /// `finalized`, no error. Answered only when the history is searched, as devnet
        /// answers an old landed signature (spec check H1).
        Landed,
        /// `finalized` with an instruction error.
        Failed,
    }

    struct Chain {
        genesis_hash: &'static str,
        holds: bool,
        status: Status,
        height: u64,
        asset_exists: bool,
        slow_send: bool,
    }

    struct Mock {
        chain: Mutex<Chain>,
        calls: Mutex<Vec<Value>>,
    }

    impl Mock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                chain: Mutex::new(Chain {
                    genesis_hash: crate::demo_field_pass::DEVNET_GENESIS,
                    holds: true,
                    status: Status::Unseen,
                    height: LAST_VALID - 1,
                    asset_exists: false,
                    slow_send: false,
                }),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn set(&self, f: impl FnOnce(&mut Chain)) {
            f(&mut self.chain.lock().unwrap());
        }

        fn count(&self, method: &str) -> usize {
            self.calls.lock().unwrap().iter().filter(|c| c["method"] == method).count()
        }

        /// The wire bytes of every `sendTransaction`, in order.
        fn sends(&self) -> Vec<Value> {
            self.calls.lock().unwrap().iter().filter(|c| c["method"] == "sendTransaction").map(|c| c["params"].clone()).collect()
        }

        /// Waits up to 2 s for `n` sends (the first is a spawned task).
        async fn wait_sends(&self, n: usize) -> Vec<Value> {
            for _ in 0..200 {
                if self.sends().len() >= n {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            self.sends()
        }
    }

    async fn mock_rpc(State(m): State<Arc<Mock>>, Json(req): Json<Value>) -> Response {
        m.calls.lock().unwrap().push(req.clone());
        let (hash, holds, status, height, exists, slow) = {
            let c = m.chain.lock().unwrap();
            (c.genesis_hash, c.holds, c.status, c.height, c.asset_exists, c.slow_send)
        };
        let ctx = json!({"slot": 1});
        let result = match req["method"].as_str().unwrap_or_default() {
            "getTokenAccountsByOwner" => {
                let accounts: Vec<Value> = holds
                    .then(|| {
                        json!({"pubkey": "11111111111111111111111111111111", "account": {"owner": TOKEN_2022_PROGRAM,
                            "data": {"program": "spl-token-2022", "parsed": {"type": "account",
                                "info": {"mint": MINT, "tokenAmount": {"amount": "1"}}}}}})
                    })
                    .into_iter()
                    .collect();
                json!({"context": ctx, "value": accounts})
            }
            "getMultipleAccounts" => json!({"context": ctx, "value": [{"owner": TOKEN_2022_PROGRAM,
                "data": [STANDARD.encode(sgt()), "base64"], "lamports": 1, "executable": false}]}),
            "getGenesisHash" => json!(hash),
            "getLatestBlockhash" => json!({"context": ctx, "value": {
                "blockhash": bs58::encode([7u8; 32]).into_string(), "lastValidBlockHeight": LAST_VALID}}),
            "sendTransaction" => {
                if slow {
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                }
                json!("SignatureTheMockNeverChecks")
            }
            "getSignatureStatuses" => {
                let searched = req["params"][1]["searchTransactionHistory"] == json!(true);
                let entry = match status {
                    Status::Unseen => Value::Null,
                    Status::Processed => json!({"slot": 1, "confirmations": 0, "err": null, "confirmationStatus": "processed"}),
                    Status::Landed if searched => json!({"slot": 1, "confirmations": null, "err": null, "confirmationStatus": "finalized"}),
                    Status::Landed => Value::Null,
                    Status::Failed => json!({"slot": 1, "confirmations": null, "err": {"InstructionError": [0, {"Custom": 9}]}, "confirmationStatus": "finalized"}),
                };
                json!({"context": ctx, "value": [entry]})
            }
            "getBlockHeight" => json!(height),
            "getAccountInfo" => {
                let value = exists.then(|| json!({"owner": crate::demo_field_pass::CORE_PROGRAM, "lamports": 1, "data": ["", "base64"], "executable": false}));
                json!({"context": ctx, "value": value})
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

    fn holder() -> String {
        bs58::encode(HOLDER).into_string()
    }

    /// Sessions `"t"` (`HOLDER`) and `"o"` (`OTHER`); both clients on one mock; the minter's
    /// seed is `[5; 32]`. `minted: false` leaves the minter off.
    async fn state_with(mock: &Arc<Mock>, minted: bool, claims: Option<Arc<dyn Claims>>) -> Arc<AppState> {
        let mut state = AppState::new(Arc::new(Config::default()));
        sign_in(&state, "t", HOLDER);
        sign_in(&state, "o", OTHER);
        let addr = serve_on_loopback(Router::new().route("/", post(mock_rpc)).with_state(Arc::clone(mock))).await;
        let url = RpcUrl::parse(&format!("http://{addr}/")).expect("the mock URL parses");
        state.genesis = Some(Arc::new(GenesisRpc::new(url.clone(), TEST_TIMEOUT, TEST_TIMEOUT).expect("the client builds")));
        if minted {
            state.field_pass.minter = Some(Arc::new(Minter::new(url, [5u8; 32], TEST_TIMEOUT, TEST_TIMEOUT).expect("the minter builds")));
        }
        if let Some(c) = claims {
            state.field_pass.claims = c;
        }
        Arc::new(state)
    }

    async fn call(state: &Arc<AppState>, method: &str, token: Option<&str>) -> (StatusCode, HeaderMap, Value) {
        let mut req = Request::builder().method(method).uri("/api/demo/field-pass");
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

    async fn post_t(state: &Arc<AppState>) -> (StatusCode, Value) {
        let (s, _, b) = call(state, "POST", Some("t")).await;
        (s, b)
    }

    async fn get_t(state: &Arc<AppState>) -> (StatusCode, Value) {
        let (s, _, b) = call(state, "GET", Some("t")).await;
        (s, b)
    }

    async fn stored(state: &Arc<AppState>) -> Option<Attempt> {
        state.field_pass.claims.get(MINT).await.unwrap()
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn fp_requires_a_session() {
        let mock = Mock::new();
        let state = state_with(&mock, true, None).await;
        let (status, _, _) = call(&state, "POST", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(mock.calls.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn fp_without_a_minter_is_503() {
        let mock = Mock::new();
        let state = state_with(&mock, false, None).await;
        assert_eq!(post_t(&state).await, (StatusCode::SERVICE_UNAVAILABLE, json!({"error": "minting not open"})));
        assert_eq!(get_t(&state).await, (StatusCode::SERVICE_UNAVAILABLE, json!({"error": "minting not open"})));
        assert_eq!(mock.calls.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn fp_without_a_genesis_token_is_403() {
        let mock = Mock::new();
        mock.set(|c| c.holds = false);
        let state = state_with(&mock, true, None).await;
        assert_eq!(post_t(&state).await, (StatusCode::FORBIDDEN, json!({"error": "no Genesis Token"})));
        assert_eq!(mock.calls.lock().unwrap().len(), 1, "the lookup's first call only");
        assert_eq!(stored(&state).await, None);
    }

    #[tokio::test]
    async fn fp_first_claim_mints_once() {
        let mock = Mock::new();
        let state = state_with(&mock, true, None).await;
        let (status, body) = post_t(&state).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let a = stored(&state).await.expect("the claim is stored");
        assert_eq!(body, json!({"asset": a.asset, "signature": a.signature, "state": "pending", "cluster": "devnet"}));
        assert_eq!(a.wallet, holder());
        assert_eq!(a.last_valid_block_height, LAST_VALID);
        let sends = mock.wait_sends(1).await;
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0][0], json!(a.wire), "the stored bytes are what was sent");
        assert_eq!(sends[0][1], json!({"encoding": "base64", "preflightCommitment": "confirmed"}));
        assert_eq!((mock.count("getGenesisHash"), mock.count("getLatestBlockhash")), (1, 1));
        let wire = STANDARD.decode(&a.wire).unwrap();
        assert_eq!(&wire[129 + 101..129 + 133], &HOLDER, "the owner is the session's wallet");
        assert!(wire.windows(FIELD_PASS_NAME.len()).any(|w| w == FIELD_PASS_NAME.as_bytes()));
        assert_eq!(state.field_pass.claims.mint_of(&holder()).await.unwrap(), Some(MINT.into()));
    }

    /// A second tap answers the stored claim and resends its bytes; it never builds again.
    #[tokio::test]
    async fn fp_second_tap_resends_the_stored_bytes() {
        let mock = Mock::new();
        let state = state_with(&mock, true, None).await;
        let (first, a) = post_t(&state).await;
        assert_eq!(first, StatusCode::CREATED);
        mock.wait_sends(1).await;
        let (second, b) = post_t(&state).await;
        assert_eq!(second, StatusCode::OK, "{b}");
        assert_eq!(b, a, "the same claim, still pending");
        let sends = mock.wait_sends(2).await;
        assert_eq!(sends.len(), 2);
        assert_eq!(sends[0][0], sends[1][0], "the resend is byte-identical");
        assert_eq!((mock.count("getGenesisHash"), mock.count("getLatestBlockhash")), (1, 1));
    }

    /// The token's pass is the first claimant's: another wallet holding it gets `409` and
    /// nothing of that claim.
    #[tokio::test]
    async fn fp_another_wallet_is_409() {
        let mock = Mock::new();
        let state = state_with(&mock, true, None).await;
        assert_eq!(post_t(&state).await.0, StatusCode::CREATED);
        let (status, _, body) = call(&state, "POST", Some("o")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, json!({"error": "this Genesis Token's pass is already claimed"}));
        assert_eq!(stored(&state).await.unwrap().wallet, holder());
        assert_eq!(mock.count("getLatestBlockhash"), 1);
        assert_eq!(state.field_pass.claims.mint_of(&bs58::encode(OTHER).into_string()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn fp_wrong_cluster_is_503() {
        let mock = Mock::new();
        mock.set(|c| c.genesis_hash = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d");
        let state = state_with(&mock, true, None).await;
        assert_eq!(post_t(&state).await, (StatusCode::SERVICE_UNAVAILABLE, json!({"error": "minting not open"})));
        assert_eq!(mock.count("getLatestBlockhash"), 0);
        assert_eq!(stored(&state).await, None);
        assert_eq!(mock.sends().len(), 0);
    }

    /// A landed mint reads `confirmed` through the history-searched status, with no resend.
    #[tokio::test]
    async fn fp_status_confirmed() {
        let mock = Mock::new();
        let state = state_with(&mock, true, None).await;
        assert_eq!(post_t(&state).await.0, StatusCode::CREATED);
        mock.wait_sends(1).await;
        mock.set(|c| c.status = Status::Landed);
        let (status, body) = get_t(&state).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["state"], "confirmed");
        assert_eq!(mock.sends().len(), 1);
        assert_eq!(mock.count("getBlockHeight"), 0);
    }

    /// `dead` needs every read; each other fixture flips one term.
    #[tokio::test]
    async fn fp_dead_needs_every_read() {
        let fixtures = [
            (Status::Unseen, LAST_VALID + 1, false, "dead", 1),
            (Status::Processed, LAST_VALID + 1, false, "pending", 1),
            (Status::Unseen, LAST_VALID, false, "pending", 2),
            (Status::Unseen, LAST_VALID + 1, true, "confirmed", 1),
            (Status::Failed, LAST_VALID - 1, false, "dead", 1),
        ];
        for (status, height, exists, want, sends) in fixtures {
            let mock = Mock::new();
            let state = state_with(&mock, true, None).await;
            assert_eq!(post_t(&state).await.0, StatusCode::CREATED);
            mock.wait_sends(1).await;
            mock.set(|c| {
                c.status = status;
                c.height = height;
                c.asset_exists = exists;
            });
            let (code, body) = get_t(&state).await;
            assert_eq!(code, StatusCode::OK, "{body}");
            assert_eq!(body["state"], want, "height {height}, exists {exists}");
            assert_eq!(mock.wait_sends(sends).await.len(), sends, "{want}: sends");
        }
    }

    /// A proven-dead claim is replaced once, by a fresh build.
    #[tokio::test]
    async fn fp_dead_claim_is_rebuilt_once() {
        let mock = Mock::new();
        let state = state_with(&mock, true, None).await;
        let (_, first) = post_t(&state).await;
        mock.wait_sends(1).await;
        mock.set(|c| c.height = LAST_VALID + 1);
        let (status, second) = post_t(&state).await;
        assert_eq!(status, StatusCode::CREATED, "{second}");
        assert_ne!(second["signature"], first["signature"]);
        assert_ne!(second["asset"], first["asset"]);
        assert_eq!(stored(&state).await.unwrap().signature, second["signature"].as_str().unwrap());
        assert_eq!(mock.count("getLatestBlockhash"), 2);
        let sends = mock.wait_sends(2).await;
        assert_eq!(sends.len(), 2);
        assert_ne!(sends[0][0], sends[1][0]);
    }

    /// The first send's reply is lost (the client times out after the mock recorded it); the
    /// status read resends the same bytes.
    #[tokio::test]
    async fn fp_dropped_send_reply_resends_the_same_bytes() {
        let mock = Mock::new();
        mock.set(|c| c.slow_send = true);
        let state = state_with(&mock, true, None).await;
        assert_eq!(post_t(&state).await.0, StatusCode::CREATED);
        assert_eq!(mock.wait_sends(1).await.len(), 1);
        let (status, body) = get_t(&state).await;
        assert_eq!((status, body["state"].clone()), (StatusCode::OK, json!("pending")));
        let sends = mock.sends();
        assert_eq!(sends.len(), 2);
        assert_eq!(sends[0][0], sends[1][0]);
        assert_eq!(mock.count("getLatestBlockhash"), 1);
    }

    /// A claim store whose first `claim` parks until released: (store, parked, release).
    fn seam() -> (Arc<dyn Claims>, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let parked = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let first = Arc::new(AtomicBool::new(true));
        let hook: PauseHook = {
            let (p, r, f) = (Arc::clone(&parked), Arc::clone(&release), Arc::clone(&first));
            Arc::new(move || {
                let (p, r, f) = (Arc::clone(&p), Arc::clone(&r), Arc::clone(&f));
                Box::pin(async move {
                    if f.swap(false, Ordering::SeqCst) {
                        p.notify_one();
                        r.notified().await;
                    }
                })
            })
        };
        (Arc::new(InMemoryClaims::with_pause_seam(hook)), parked, release)
    }

    /// Two taps race: the first parks before its claim write, the second claims and sends,
    /// the first then loses its write and answers the winner's claim. One claim, one send.
    #[tokio::test]
    async fn fp_two_taps_race_one_claim() {
        let (claims, parked, release) = seam();
        let mock = Mock::new();
        let state = state_with(&mock, true, Some(claims)).await;
        let a = tokio::spawn({
            let state = Arc::clone(&state);
            async move { post_t(&state).await }
        });
        parked.notified().await;
        let (b_status, b) = post_t(&state).await;
        assert_eq!(b_status, StatusCode::CREATED, "{b}");
        release.notify_one();
        let (a_status, a) = a.await.unwrap();
        assert_eq!(a_status, StatusCode::OK, "{a}");
        assert_eq!(a, b, "the parked tap answers the winner's claim");
        assert_eq!(stored(&state).await.unwrap().signature, b["signature"].as_str().unwrap());
        assert_eq!(mock.count("getLatestBlockhash"), 2, "both built");
        mock.wait_sends(1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(mock.sends().len(), 1, "only the winner sent");
    }

    /// Two wallets holding one token race: the holder parks before its write, the other
    /// wallet claims, and the holder's lost write answers `409` (`lost_write`'s own check).
    #[tokio::test]
    async fn fp_race_lost_to_another_wallet_is_409() {
        let (claims, parked, release) = seam();
        let mock = Mock::new();
        let state = state_with(&mock, true, Some(claims)).await;
        let a = tokio::spawn({
            let state = Arc::clone(&state);
            async move { post_t(&state).await }
        });
        parked.notified().await;
        let (o_status, _, o) = call(&state, "POST", Some("o")).await;
        assert_eq!(o_status, StatusCode::CREATED, "{o}");
        release.notify_one();
        let (a_status, a) = a.await.unwrap();
        assert_eq!(a_status, StatusCode::CONFLICT, "{a}");
        assert_eq!(a, json!({"error": "this Genesis Token's pass is already claimed"}));
        assert_eq!(stored(&state).await.unwrap().wallet, bs58::encode(OTHER).into_string());
        assert_eq!(state.field_pass.claims.mint_of(&holder()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn fp_get_without_a_claim_is_404() {
        let mock = Mock::new();
        let state = state_with(&mock, true, None).await;
        assert_eq!(get_t(&state).await, (StatusCode::NOT_FOUND, json!({"error": "no field pass claim"})));
        assert_eq!(mock.calls.lock().unwrap().len(), 0);
    }

    /// 3 a minute per wallet (a literal): the 4th is `429`. No minter, so the first 3 are `503`.
    #[tokio::test]
    async fn fp_post_wallet_limit() {
        let mock = Mock::new();
        let state = state_with(&mock, false, None).await;
        let mut statuses = Vec::new();
        for _ in 0..4 {
            statuses.push(post_t(&state).await.0);
        }
        let mut want = vec![StatusCode::SERVICE_UNAVAILABLE; 3];
        want.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, want);
    }

    /// 4 units of chain work already spent this window (a literal): the next mint is `503`
    /// with `Retry-After`, and nothing reaches the mock.
    #[tokio::test]
    async fn fp_chain_ceiling_is_503_busy() {
        let mock = Mock::new();
        let state = state_with(&mock, true, None).await;
        let now = Instant::now();
        for _ in 0..4 {
            state.field_pass.limits.chain.check((), now).expect("under the ceiling");
        }
        let (status, headers, body) = call(&state, "POST", Some("t")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(headers.contains_key(header::RETRY_AFTER));
        assert_eq!(body, json!({"error": "the pass desk is busy, retry shortly"}));
        assert_eq!(mock.calls.lock().unwrap().len(), 0);
    }
}
