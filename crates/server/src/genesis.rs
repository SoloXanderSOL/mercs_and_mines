//! The Seeker Genesis Token (SGT) lookup (BP-4, keeper). Read-only, mainnet, hand-rolled
//! JSON-RPC over `reqwest` (ODQ-52 (a)). The recipe is Solana Mobile's
//! `@solana-mobile/skr-genesis-token@1.0.1`: list the owner's Token-2022 accounts, drop the
//! zero balances, fetch the mints, and accept a mint whose `MetadataPointer` (type 18) and
//! `TokenGroupMember` (type 23) both name the SGT group. It gates nothing (ODQ-53 (a)).
//!
//! The RPC URL carries a provider key, so it never reaches a log: `RpcUrl`'s `Debug` redacts
//! it, and every `reqwest::Error` goes through `rpc_error_text`, which strips the URL.

use std::error::Error as _;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};

/// The Token-2022 program, owner of every SGT mint and token account.
pub const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
/// The SGT metadata and group address (docs.solanamobile.com).
pub const SGT_GROUP: &str = "GT22s89nU4iWFkNXj1Bw6uYhJJWDRPpShHt4Bk8f99Te";
/// Mints checked per lookup: one `getMultipleAccounts` call. A wallet with more non-zero
/// Token-2022 mints is checked on the first 100 in string order.
pub const MAX_MINTS: usize = 100;
/// The largest RPC response body read; a larger one fails the lookup.
pub const MAX_RPC_BODY: usize = 1024 * 1024;
pub const RPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Per call, send plus body.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(3);

const EXT_METADATA_POINTER: u16 = 18;
const EXT_GROUP_MEMBER: u16 = 23;
/// Byte 165 of a Token-2022 account holds its type (1 = mint); the TLV extensions follow.
const ACCOUNT_TYPE_AT: usize = 165;
const TLV_START: usize = 166;

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

/// The lookup client: one connection pool, no redirects, no proxy.
pub struct GenesisRpc {
    client: reqwest::Client,
    url: RpcUrl,
}

/// The client `AppState` holds. `None` when no URL is configured, or when the client fails
/// to build (journalled); `GET /api/genesis` then answers `503`.
pub fn from_config(url: Option<&RpcUrl>) -> Option<Arc<GenesisRpc>> {
    let url = url?.clone();
    match GenesisRpc::new(url, RPC_CONNECT_TIMEOUT, RPC_TIMEOUT) {
        Ok(rpc) => {
            tracing::info!("genesis lookup: on");
            Some(Arc::new(rpc))
        }
        Err(e) => {
            tracing::error!("genesis lookup: the RPC client failed to build ({e}); GET /api/genesis answers 503");
            None
        }
    }
}

impl GenesisRpc {
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

    /// The SGT lookup for `owner` (base58). `Ok(None)`: no SGT. `Ok(Some(mint))`: the
    /// smallest SGT mint in string order. `Err`: the envelope failed (transport, timeout,
    /// non-2xx, a JSON-RPC error, an unreadable or oversized body); the text holds no URL.
    /// A mint that can't be read is "not an SGT", never an error.
    pub async fn lookup(&self, owner: &str) -> Result<Option<String>, String> {
        let accounts = self
            .call(
                "getTokenAccountsByOwner",
                json!([owner, {"programId": TOKEN_2022_PROGRAM}, {"commitment": "confirmed", "encoding": "jsonParsed"}]),
            )
            .await?;
        let mints = held_mints(&accounts)?;
        if mints.is_empty() {
            return Ok(None);
        }
        let infos = self
            .call("getMultipleAccounts", json!([&mints, {"commitment": "confirmed", "encoding": "base64"}]))
            .await?;
        let values = infos["value"].as_array().ok_or("getMultipleAccounts: result.value is not an array")?;
        if values.len() != mints.len() {
            return Err(format!("getMultipleAccounts: {} accounts for {} mints", values.len(), mints.len()));
        }
        let sgts: Vec<&String> =
            mints.iter().zip(values).filter(|(_, account)| is_sgt_account(account)).map(|(mint, _)| mint).collect();
        Ok(sgts.into_iter().min().cloned())
    }

    /// One JSON-RPC call; returns its `result`. The body is read up to `MAX_RPC_BODY`.
    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let mut resp = self
            .client
            .post(self.url.0.clone())
            .json(&request)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| format!("{method}: {}", rpc_error_text(e)))?;
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| format!("{method}: {}", rpc_error_text(e)))? {
            if body.len() + chunk.len() > MAX_RPC_BODY {
                return Err(format!("{method}: response body over {MAX_RPC_BODY} bytes"));
            }
            body.extend_from_slice(&chunk);
        }
        let reply: Value = serde_json::from_slice(&body).map_err(|e| format!("{method}: body is not JSON ({e})"))?;
        if let Some(error) = reply.get("error").filter(|e| !e.is_null()) {
            return Err(format!("{method}: JSON-RPC error {error}"));
        }
        reply.get("result").cloned().ok_or_else(|| format!("{method}: reply has no result"))
    }
}

/// The distinct mints of the non-zero accounts in a `getTokenAccountsByOwner` result, in
/// string order, at most `MAX_MINTS`. An element without a mint or an amount is an error.
fn held_mints(result: &Value) -> Result<Vec<String>, String> {
    let list = result["value"].as_array().ok_or("getTokenAccountsByOwner: result.value is not an array")?;
    let mut mints = std::collections::BTreeSet::new();
    for account in list {
        let info = &account["account"]["data"]["parsed"]["info"];
        let mint = info["mint"].as_str().ok_or("getTokenAccountsByOwner: an account has no mint")?;
        let amount = info["tokenAmount"]["amount"].as_str().ok_or("getTokenAccountsByOwner: an account has no amount")?;
        if amount != "0" {
            mints.insert(mint.to_string());
        }
    }
    Ok(mints.into_iter().take(MAX_MINTS).collect())
}

/// One `getMultipleAccounts` element (`null`, or an account with base64 data).
fn is_sgt_account(account: &Value) -> bool {
    let Some(owner) = account["owner"].as_str() else { return false };
    let Some(encoded) = account["data"][0].as_str() else { return false };
    let Ok(data) = base64::engine::general_purpose::STANDARD.decode(encoded) else { return false };
    is_sgt_mint(owner, &data)
}

/// True when `data` is a Token-2022 mint whose `MetadataPointer` and `TokenGroupMember`
/// both name `SGT_GROUP` (bytes 32..64 of each value). Anything unreadable is `false`, and
/// nothing here panics. The walk ends at type 0 or at the end of the data: the real SGT
/// mint has no terminator.
pub fn is_sgt_mint(owner: &str, data: &[u8]) -> bool {
    if owner != TOKEN_2022_PROGRAM {
        return false;
    }
    if data.get(ACCOUNT_TYPE_AT) != Some(&1) {
        return false;
    }
    let Ok(group) = bs58::decode(SGT_GROUP).into_vec() else { return false };
    let mut pointer_ok = false;
    let mut member_ok = false;
    let mut at = TLV_START;
    while let Some(head) = data.get(at..at + 4) {
        let kind = u16::from_le_bytes([head[0], head[1]]);
        let len = usize::from(u16::from_le_bytes([head[2], head[3]]));
        if kind == 0 {
            break;
        }
        let Some(value) = data.get(at + 4..at + 4 + len) else { return false };
        match kind {
            EXT_METADATA_POINTER if len >= 64 => pointer_ok = value[32..64] == group[..],
            EXT_GROUP_MEMBER if len >= 72 => member_ok = value[32..64] == group[..],
            _ => {}
        }
        at += 4 + len;
    }
    pointer_ok && member_ok
}

/// Synthetic mint accounts for tests. Never real chain data (a real mint names its holder).
#[cfg(test)]
pub(crate) mod fixtures {
    /// A group that is not the SGT's.
    pub(crate) const OTHER_GROUP: [u8; 32] = [9u8; 32];

    /// A Token-2022 mint: 165 zero bytes, the account type (1 = mint), then each extension
    /// as `(type u16 LE, len u16 LE, value)`, with no terminator.
    pub(crate) fn mint(exts: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut data = vec![0u8; 165];
        data.push(1);
        for (kind, value) in exts {
            data.extend_from_slice(&kind.to_le_bytes());
            data.extend_from_slice(&(value.len() as u16).to_le_bytes());
            data.extend_from_slice(value);
        }
        data
    }

    /// `len` bytes whose bytes 32..64 are `group`.
    pub(crate) fn naming(len: usize, group: &[u8]) -> Vec<u8> {
        let mut value = vec![7u8; len];
        value[32..64].copy_from_slice(group);
        value
    }

    pub(crate) fn sgt_group() -> Vec<u8> {
        bs58::decode(super::SGT_GROUP).into_vec().expect("SGT_GROUP is base58")
    }

    /// The real SGT mint's layout (spike Q2): (18,64) (12,32) (3,32) (22,64) (23,72), 450 bytes.
    pub(crate) fn sgt_like(pointer_group: &[u8], member_group: &[u8]) -> Vec<u8> {
        mint(&[
            (18, naming(64, pointer_group)),
            (12, vec![0u8; 32]),
            (3, vec![0u8; 32]),
            (22, vec![0u8; 64]),
            (23, naming(72, member_group)),
        ])
    }

    pub(crate) fn sgt() -> Vec<u8> {
        let g = sgt_group();
        sgt_like(&g, &g)
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{mint, naming, sgt, sgt_group, sgt_like, OTHER_GROUP};
    use super::{is_sgt_mint, GenesisRpc, RpcUrl, RPC_CONNECT_TIMEOUT, RPC_TIMEOUT, TOKEN_2022_PROGRAM};

    #[test]
    fn sgt_mint_with_the_real_layout_is_an_sgt() {
        let data = sgt();
        assert_eq!(data.len(), 450, "the spike's SGT mint is 450 bytes");
        assert!(is_sgt_mint(TOKEN_2022_PROGRAM, &data));
    }

    #[test]
    fn metadata_pointer_to_another_group_is_not_an_sgt() {
        assert!(!is_sgt_mint(TOKEN_2022_PROGRAM, &sgt_like(&OTHER_GROUP, &sgt_group())));
    }

    #[test]
    fn group_member_of_another_group_is_not_an_sgt() {
        assert!(!is_sgt_mint(TOKEN_2022_PROGRAM, &sgt_like(&sgt_group(), &OTHER_GROUP)));
    }

    #[test]
    fn malformed_mints_are_not_sgts_and_never_panic() {
        let good = sgt();
        assert!(is_sgt_mint(TOKEN_2022_PROGRAM, &good), "control: the base fixture is an SGT");
        let mut not_a_mint = good.clone();
        not_a_mint[165] = 2;
        let truncated = good[..good.len() - 1].to_vec();
        let mut terminated_first = mint(&[(0, Vec::new())]);
        terminated_first.extend_from_slice(&good[166..]);
        let short_pointer = mint(&[(18, vec![0u8; 63]), (23, naming(72, &sgt_group()))]);
        let cases: Vec<(&str, &str, Vec<u8>)> = vec![
            ("empty", TOKEN_2022_PROGRAM, Vec::new()),
            ("165 bytes, no account type", TOKEN_2022_PROGRAM, vec![0u8; 165]),
            ("account type 2", TOKEN_2022_PROGRAM, not_a_mint),
            ("legacy SPL Token owner", "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", good.clone()),
            ("last extension runs past the end", TOKEN_2022_PROGRAM, truncated),
            ("type 0 before the extensions", TOKEN_2022_PROGRAM, terminated_first),
            ("type 18 shorter than 64", TOKEN_2022_PROGRAM, short_pointer),
        ];
        for (name, owner, data) in cases {
            assert!(!is_sgt_mint(owner, &data), "{name} must not read as an SGT");
        }
    }

    #[tokio::test]
    async fn genesis_rpc_client_builds() {
        let url = RpcUrl::parse("https://rpc.example/?api-key=SECRETKEY").expect("an https URL parses");
        assert!(GenesisRpc::new(url, RPC_CONNECT_TIMEOUT, RPC_TIMEOUT).is_ok());
    }
}
