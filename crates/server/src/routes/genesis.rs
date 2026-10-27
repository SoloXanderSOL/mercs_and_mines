use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{ConnectInfo, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;

use crate::auth::AuthSession;
use crate::rate_limit::GENESIS_ALL_PER_MINUTE;
use crate::state::AppState;

use super::{bad_gateway, client_ip, service_unavailable, too_many_requests, AppError};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/genesis", get(get_genesis))
}

/// The Genesis Token answer. `mint` is the smallest SGT mint the wallet holds, if any.
#[derive(Serialize)]
pub struct GenesisView {
    pub holds_genesis_token: bool,
    pub mint: Option<String>,
}

/// Does the signed-in wallet hold a Seeker Genesis Token? `GET /api/genesis` (BP-4, keeper).
/// Order: `401` (no session) → `429` per address (`genesis_ip`) → `429` per wallet
/// (`genesis_wallet`) → `503` when no RPC URL is configured → `503` over the global ceiling
/// (`genesis_all`, the bound on the provider key's quota; journalled through `RateWarn`) →
/// the lookup → `200`, or `502` with one `warn!` naming the cause (never the URL).
/// - The wallet is always the session's; no request names another.
/// - It gates nothing (ODQ-53 (a)): the client shows a badge or the courtesy copy.
/// - Read-only: it writes no row and no key.
async fn get_genesis(
    AuthSession(token): AuthSession,
    State(state): State<Arc<AppState>>,
    ci: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let ip = client_ip(ci, &headers, &state.rate_limits)?;
    let now = Instant::now();
    if let Err(r) = state.rate_limits.genesis_ip.check(ip, now) {
        return Ok(too_many_requests(r));
    }
    if let Err(r) = state.rate_limits.genesis_wallet.check(token.wallet_address.clone(), now) {
        return Ok(too_many_requests(r));
    }
    let Some(rpc) = state.genesis.as_ref() else {
        return Err(service_unavailable("genesis lookup unavailable"));
    };
    if state.rate_limits.genesis_all.check((), now).is_err() {
        if let Some(n) = state.rate_limits.genesis_all_warn.hit(now) {
            tracing::warn!(refused = n, "genesis lookups are over the global ceiling ({GENESIS_ALL_PER_MINUTE}/min); answering 503");
        }
        return Err(service_unavailable("genesis lookup unavailable"));
    }
    match rpc.lookup(&token.wallet_address).await {
        Ok(mint) => Ok(Json(GenesisView { holds_genesis_token: mint.is_some(), mint }).into_response()),
        Err(e) => {
            tracing::warn!("genesis lookup failed: {e}");
            Err(bad_gateway("genesis lookup failed"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::genesis::fixtures::{mint, sgt};
    use crate::genesis::{GenesisRpc, TOKEN_2022_PROGRAM};
    use crate::json_rpc::RpcUrl;
    use crate::routes::{serve_on_loopback, with_test_peer, TEST_PEER};
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use axum::routing::post;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;
    use tower::ServiceExt;

    /// In-memory sessions only (no DB): bytes 221–227 sit above every byte in use.
    const HOLDER: [u8; 32] = [221u8; 32];
    /// The mock's client: both timeouts. The slow mock waits 1,000 ms.
    const TEST_TIMEOUT: Duration = Duration::from_millis(250);

    // ── The mock RPC ──────────────────────────────────────────────────────────

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Normal,
        /// `getTokenAccountsByOwner` answers after 1,000 ms.
        SlowFirst,
        Http500,
        RpcError,
        /// Valid JSON (no accounts) padded past 1 MiB.
        Huge,
    }

    struct Mock {
        mode: Mode,
        /// `(mint, amount)` for each account `getTokenAccountsByOwner` returns.
        accounts: Vec<(String, String)>,
        /// Mint data by mint for `getMultipleAccounts`; a mint not here answers `null`.
        mints: HashMap<String, Vec<u8>>,
        /// Every request body, in order.
        calls: Mutex<Vec<Value>>,
    }

    impl Mock {
        fn new(mode: Mode, accounts: &[(&str, &str)], mints: &[(&str, Vec<u8>)]) -> Arc<Self> {
            Arc::new(Self {
                mode,
                accounts: accounts.iter().map(|(m, a)| (m.to_string(), a.to_string())).collect(),
                mints: mints.iter().map(|(m, d)| (m.to_string(), d.clone())).collect(),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> Vec<Value> {
            self.calls.lock().unwrap().clone()
        }
    }

    async fn mock_rpc(State(m): State<Arc<Mock>>, Json(req): Json<Value>) -> Response {
        m.calls.lock().unwrap().push(req.clone());
        let method = req["method"].as_str().unwrap_or_default().to_string();
        match m.mode {
            Mode::Http500 => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            Mode::RpcError => {
                return Json(json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32005, "message": "node is behind"}}))
                    .into_response();
            }
            Mode::Huge => {
                let mut body = br#"{"jsonrpc":"2.0","id":1,"result":{"value":[]}}"#.to_vec();
                body.resize(1024 * 1024 + 1, b' ');
                return ([(header::CONTENT_TYPE, "application/json")], body).into_response();
            }
            Mode::SlowFirst if method == "getTokenAccountsByOwner" => {
                tokio::time::sleep(Duration::from_millis(1000)).await;
            }
            _ => {}
        }
        let value: Vec<Value> = match method.as_str() {
            "getTokenAccountsByOwner" => m
                .accounts
                .iter()
                .map(|(mint, amount)| {
                    json!({
                        "pubkey": "11111111111111111111111111111111",
                        "account": {
                            "owner": TOKEN_2022_PROGRAM,
                            "data": {
                                "program": "spl-token-2022",
                                "parsed": {"type": "account", "info": {"mint": mint, "tokenAmount": {"amount": amount}}},
                            },
                        },
                    })
                })
                .collect(),
            "getMultipleAccounts" => req["params"][0]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|asked| match asked.as_str().and_then(|s| m.mints.get(s)) {
                    Some(data) => json!({"owner": TOKEN_2022_PROGRAM, "data": [STANDARD.encode(data), "base64"], "lamports": 1, "executable": false}),
                    None => Value::Null,
                })
                .collect(),
            _ => return StatusCode::BAD_REQUEST.into_response(),
        };
        Json(json!({"jsonrpc": "2.0", "id": 1, "result": {"context": {"slot": 1}, "value": value}})).into_response()
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

    /// Session `"t"` for `HOLDER`; with a mock, a lookup client pointed at it whose URL
    /// carries `api-key=SECRETKEY`.
    async fn state_with(mock: Option<&Arc<Mock>>) -> Arc<AppState> {
        let mut state = AppState::new(Arc::new(Config::default()));
        sign_in(&state, "t", HOLDER);
        if let Some(m) = mock {
            let addr = serve_on_loopback(Router::new().route("/", post(mock_rpc)).with_state(Arc::clone(m))).await;
            let url = RpcUrl::parse(&format!("http://{addr}/?api-key=SECRETKEY")).expect("the mock URL parses");
            state.genesis = Some(Arc::new(GenesisRpc::new(url, TEST_TIMEOUT, TEST_TIMEOUT).expect("the client builds")));
        }
        Arc::new(state)
    }

    async fn get(state: &Arc<AppState>, token: Option<&str>) -> (StatusCode, Value) {
        let mut req = Request::builder().method("GET").uri("/api/genesis");
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let resp = with_test_peer(router().with_state(Arc::clone(state)), TEST_PEER)
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
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

    /// The mock holds an SGT for anyone, so a missing session check would answer `200`.
    #[tokio::test]
    async fn genesis_requires_a_session() {
        let mock = Mock::new(Mode::Normal, &[("MintSgtOne", "1")], &[("MintSgtOne", sgt())]);
        let state = state_with(Some(&mock)).await;
        let (status, _) = get(&state, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(mock.calls().len(), 0, "no RPC call before the session check");
    }

    #[tokio::test]
    async fn genesis_without_an_rpc_url_is_503() {
        let state = state_with(None).await;
        let (status, body) = get(&state, Some("t")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body, json!({"error": "genesis lookup unavailable"}));
    }

    /// Seven wallets in turn, so no wallet passes 3 and the 21st request meets the address
    /// limit. No URL: the first 20 answer `503`.
    #[tokio::test]
    async fn genesis_per_ip_limit() {
        let state = state_with(None).await;
        for i in 0..7u8 {
            sign_in(&state, &format!("w{i}"), [221 + i; 32]);
        }
        let mut statuses = Vec::new();
        for i in 0..21u8 {
            statuses.push(get(&state, Some(&format!("w{}", i % 7))).await.0);
        }
        let mut expected = vec![StatusCode::SERVICE_UNAVAILABLE; 20];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected);
    }

    #[tokio::test]
    async fn genesis_per_wallet_limit() {
        let state = state_with(None).await;
        let mut statuses = Vec::new();
        for _ in 0..4 {
            statuses.push(get(&state, Some("t")).await.0);
        }
        let mut expected = vec![StatusCode::SERVICE_UNAVAILABLE; 3];
        expected.push(StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses, expected);
    }

    #[tokio::test]
    async fn genesis_holder_gets_the_mint() {
        let mock = Mock::new(Mode::Normal, &[("MintSgtOne", "1")], &[("MintSgtOne", sgt())]);
        let state = state_with(Some(&mock)).await;
        let (status, body) = get(&state, Some("t")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"holds_genesis_token": true, "mint": "MintSgtOne"}));
        let calls = mock.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["method"], "getTokenAccountsByOwner");
        assert_eq!(calls[0]["params"][0], bs58::encode(HOLDER).into_string());
        assert_eq!(calls[0]["params"][1]["programId"], TOKEN_2022_PROGRAM);
        assert_eq!(calls[0]["params"][2]["encoding"], "jsonParsed");
        assert_eq!(calls[1]["method"], "getMultipleAccounts");
        assert_eq!(calls[1]["params"][0], json!(["MintSgtOne"]));
        assert_eq!(calls[1]["params"][1]["encoding"], "base64");
    }

    /// The only account holds 0 of a mint the mock would answer as an SGT.
    #[tokio::test]
    async fn genesis_zero_balance_is_not_held() {
        let mock = Mock::new(Mode::Normal, &[("MintSgtZero", "0")], &[("MintSgtZero", sgt())]);
        let state = state_with(Some(&mock)).await;
        let (status, body) = get(&state, Some("t")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"holds_genesis_token": false, "mint": null}));
        assert_eq!(mock.calls().len(), 1, "no mint to fetch, so no second call");
    }

    #[tokio::test]
    async fn genesis_picks_the_smallest_sgt_mint() {
        let mock = Mock::new(
            Mode::Normal,
            &[("MintSgtD", "1"), ("MintPlainA", "1"), ("MintSgtB", "1")],
            &[("MintSgtD", sgt()), ("MintPlainA", mint(&[])), ("MintSgtB", sgt())],
        );
        let state = state_with(Some(&mock)).await;
        let (status, body) = get(&state, Some("t")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"holds_genesis_token": true, "mint": "MintSgtB"}));
    }

    /// The first call outlasts the client's 250 ms; the reply would name an SGT.
    #[tokio::test]
    async fn genesis_rpc_timeout_is_502() {
        let mock = Mock::new(Mode::SlowFirst, &[("MintSgtOne", "1")], &[("MintSgtOne", sgt())]);
        let state = state_with(Some(&mock)).await;
        let (status, body) = get(&state, Some("t")).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body, json!({"error": "genesis lookup failed"}));
    }

    /// The journal names the cause and never the URL's key.
    #[tokio::test]
    async fn genesis_rpc_http_error_is_502_and_journals_no_key() {
        let mock = Mock::new(Mode::Http500, &[], &[]);
        let state = state_with(Some(&mock)).await;
        let buf = Buf::default();
        let w = buf.clone();
        let sub = tracing_subscriber::fmt().with_writer(move || w.clone()).with_ansi(false).finish();
        let guard = tracing::subscriber::set_default(sub);
        let (status, body) = get(&state, Some("t")).await;
        drop(guard);
        let journal = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body, json!({"error": "genesis lookup failed"}));
        assert!(journal.contains("500 Internal Server Error"), "the journal lost the cause: {journal}");
        assert!(!journal.contains("SECRETKEY"), "the journal leaked the key: {journal}");
    }

    #[tokio::test]
    async fn genesis_rpc_error_object_is_502() {
        let mock = Mock::new(Mode::RpcError, &[], &[]);
        let state = state_with(Some(&mock)).await;
        let (status, body) = get(&state, Some("t")).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body, json!({"error": "genesis lookup failed"}));
    }

    /// 120 lookups already spent this minute (a literal, not the const): the next is `503`
    /// and never reaches the mock, which would answer an SGT.
    #[tokio::test]
    async fn genesis_global_ceiling_is_503() {
        let mock = Mock::new(Mode::Normal, &[("MintSgtOne", "1")], &[("MintSgtOne", sgt())]);
        let state = state_with(Some(&mock)).await;
        let now = Instant::now();
        for _ in 0..120 {
            state.rate_limits.genesis_all.check((), now).expect("under the ceiling");
        }
        let (status, body) = get(&state, Some("t")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body, json!({"error": "genesis lookup unavailable"}));
        assert_eq!(mock.calls().len(), 0);
    }

    /// A valid reply 1 byte past 1 MiB (a literal) is refused, not parsed.
    #[tokio::test]
    async fn genesis_oversized_rpc_body_is_502() {
        let mock = Mock::new(Mode::Huge, &[], &[]);
        let state = state_with(Some(&mock)).await;
        let (status, body) = get(&state, Some("t")).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    }

    /// 101 non-zero mints: one call asks for the first 100 in string order.
    #[tokio::test]
    async fn genesis_checks_at_most_100_mints() {
        let names: Vec<String> = (0..101).map(|i| format!("M{i:03}")).collect();
        let accounts: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "1")).collect();
        let mock = Mock::new(Mode::Normal, &accounts, &[]);
        let state = state_with(Some(&mock)).await;
        let (status, body) = get(&state, Some("t")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let calls = mock.calls();
        assert_eq!(calls.len(), 2);
        let asked = calls[1]["params"][0].as_array().expect("a mint list").clone();
        assert_eq!(asked.len(), 100);
        assert_eq!(asked[0], "M000");
        assert_eq!(asked[99], "M099");
    }
}
