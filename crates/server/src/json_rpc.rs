//! The outbound Solana JSON-RPC transport (BP-5, keeper; extracted from BP-4's `genesis.rs`).
//! One `reqwest` client per caller: connect and per-call timeouts, no redirects, no proxy, and
//! a body cap read through `chunk()`. The Genesis Token lookup and the checkout rail use it.
//!
//! An RPC URL may carry a provider key, so it never reaches a log: `RpcUrl`'s `Debug` redacts
//! it, and every `reqwest::Error` goes through `rpc_error_text`, which strips the URL.

use std::error::Error as _;
use std::time::Duration;

use serde_json::{json, Value};

/// The largest RPC response body read; a larger one fails the call.
pub const MAX_RPC_BODY: usize = 1024 * 1024;
pub const RPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Per call, send plus body.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(3);
/// The most of a JSON-RPC `error` object a message carries (Reviewer BP-4 F5).
pub const MAX_RPC_ERROR_CHARS: usize = 512;

/// A JSON-RPC endpoint URL that may carry a provider key. `Debug` never prints it.
#[derive(Clone)]
pub struct RpcUrl(reqwest::Url);

impl RpcUrl {
    /// An `http` or `https` URL; anything else is `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        let url = reqwest::Url::parse(raw).ok()?;
        if matches!(url.scheme(), "http" | "https") {
            Some(Self(url))
        } else {
            None
        }
    }
}

impl std::fmt::Debug for RpcUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RpcUrl(<redacted>)")
    }
}

/// A `reqwest::Error` as text, URL stripped, followed by its source chain (the cause: a
/// timeout, a refused connection). Never format a `reqwest::Error` any other way.
pub fn rpc_error_text(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut text = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        text.push_str(": ");
        text.push_str(&c.to_string());
        cause = c.source();
    }
    text
}

/// Why a call failed. Both texts are URL-free.
#[derive(Debug, PartialEq)]
pub enum RpcError {
    /// The endpoint refused for rate: HTTP `429`, or a JSON-RPC `error` with code `429`.
    /// Retrying at once extends the lockout (the public devnet counts refused calls).
    RateLimited(String),
    /// Anything else: transport, timeout, another non-2xx, another JSON-RPC error, a body
    /// that is oversized or not JSON, a reply without a `result`.
    Failed(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::RateLimited(t) => write!(f, "rate limited: {t}"),
            RpcError::Failed(t) => f.write_str(t),
        }
    }
}

impl From<RpcError> for String {
    fn from(e: RpcError) -> String {
        e.to_string()
    }
}

/// One JSON-RPC endpoint: one connection pool, no redirects, no proxy.
pub struct JsonRpc {
    client: reqwest::Client,
    url: RpcUrl,
}

impl JsonRpc {
    pub fn new(url: RpcUrl, connect_timeout: Duration, timeout: Duration) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(rpc_error_text)?;
        Ok(Self { client, url })
    }

    /// One JSON-RPC call; returns its `result`. The body is read up to `MAX_RPC_BODY`.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let sent = self.client.post(self.url.0.clone()).json(&request).send().await;
        let mut resp = match sent {
            Ok(r) if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => {
                return Err(RpcError::RateLimited(format!("{method}: HTTP 429")));
            }
            Ok(r) => r.error_for_status().map_err(|e| RpcError::Failed(format!("{method}: {}", rpc_error_text(e))))?,
            Err(e) => return Err(RpcError::Failed(format!("{method}: {}", rpc_error_text(e)))),
        };
        let mut body = Vec::new();
        while let Some(chunk) =
            resp.chunk().await.map_err(|e| RpcError::Failed(format!("{method}: {}", rpc_error_text(e))))?
        {
            if body.len() + chunk.len() > MAX_RPC_BODY {
                return Err(RpcError::Failed(format!("{method}: response body over {MAX_RPC_BODY} bytes")));
            }
            body.extend_from_slice(&chunk);
        }
        let reply: Value =
            serde_json::from_slice(&body).map_err(|e| RpcError::Failed(format!("{method}: body is not JSON ({e})")))?;
        if let Some(error) = reply.get("error").filter(|e| !e.is_null()) {
            let text: String = error.to_string().chars().take(MAX_RPC_ERROR_CHARS).collect();
            if error["code"].as_i64() == Some(429) {
                return Err(RpcError::RateLimited(format!("{method}: JSON-RPC error {text}")));
            }
            return Err(RpcError::Failed(format!("{method}: JSON-RPC error {text}")));
        }
        reply.get("result").cloned().ok_or_else(|| RpcError::Failed(format!("{method}: reply has no result")))
    }
}

#[cfg(test)]
mod tests {
    use super::{JsonRpc, RpcError, RpcUrl, RPC_CONNECT_TIMEOUT, RPC_TIMEOUT};
    use crate::routes::serve_on_loopback;
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{json, Value};

    #[tokio::test]
    async fn json_rpc_client_builds() {
        let url = RpcUrl::parse("https://rpc.example/?api-key=SECRETKEY").expect("an https URL parses");
        assert!(JsonRpc::new(url, RPC_CONNECT_TIMEOUT, RPC_TIMEOUT).is_ok());
    }

    /// A one-route mock that always gives the same answer.
    async fn client_for(reply: fn() -> Response) -> JsonRpc {
        let app = Router::new().route("/", post(move |_: Json<Value>| async move { reply() }));
        let addr = serve_on_loopback(app).await;
        let url = RpcUrl::parse(&format!("http://{addr}/?api-key=SECRETKEY")).expect("the mock URL parses");
        JsonRpc::new(url, RPC_CONNECT_TIMEOUT, RPC_TIMEOUT).expect("the client builds")
    }

    /// Both forms of "too many requests" are `RateLimited`; another error code is `Failed`.
    #[tokio::test]
    async fn json_rpc_rate_limits_are_told_apart() {
        let http = client_for(|| StatusCode::TOO_MANY_REQUESTS.into_response()).await;
        assert!(matches!(http.call("getSlot", json!([])).await, Err(RpcError::RateLimited(_))));
        let rpc = client_for(|| {
            Json(json!({"jsonrpc": "2.0", "id": 1, "error": {"code": 429, "message": "Too many requests"}})).into_response()
        })
        .await;
        assert!(matches!(rpc.call("getSlot", json!([])).await, Err(RpcError::RateLimited(_))));
        let other = client_for(|| {
            Json(json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32005, "message": "node is behind"}})).into_response()
        })
        .await;
        assert!(matches!(other.call("getSlot", json!([])).await, Err(RpcError::Failed(_))));
    }

    /// A 2,000-character JSON-RPC error message reaches the text cut to 512 characters.
    #[tokio::test]
    async fn json_rpc_error_text_is_truncated() {
        let rpc = client_for(|| {
            Json(json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "x".repeat(2000)}})).into_response()
        })
        .await;
        let Err(RpcError::Failed(text)) = rpc.call("getSlot", json!([])).await else { panic!("a Failed error") };
        let xs = text.chars().filter(|c| *c == 'x').count();
        assert!(xs > 400 && xs < 512, "{xs} x's of 2,000 reached the text");
    }
}
