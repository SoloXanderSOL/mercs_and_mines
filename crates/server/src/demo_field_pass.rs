//! The Breakpoint Genesis Field Pass mint (BP-7a, demo-only; ODQ-70 (a), ODQ-74 (a1)–(d)).
//! A free soulbound Metaplex Core asset on **devnet**, one per Seeker Genesis Token, minted
//! by a server-held key that is both fee payer and update authority. The real mint is
//! GAP-22's, on the mandated client, after the booth; nothing here is ever ported to `dev`.
//!
//! - **The transaction is hand-built** (no Solana crate): a v0 message holding one Core
//!   `CreateV2` instruction with the `PermanentFreezeDelegate { frozen: true }` plugin and
//!   authority `None`, the shape the ODQ-70 spike sent (`kxb9…`, `3d6e…`; ODQ-74's
//!   "CreateV1 + legacy" was a misnaming, corrected at BP-7a). The oracle tests pin it to
//!   the spike's bytes.
//! - **The claim is the signed attempt.** It is written once, with `SET NX`, before anything
//!   is sent, keyed by the Genesis Token's mint. A resend is always the stored bytes. A
//!   rebuild happens only once the attempt is proven dead: the blockhash has expired at
//!   `finalized`, a history-searched status is `null` or a failure, and the asset account
//!   does not exist (`chain_state`).
//! - **`Attempt` is a persisted Redis type** (`demo:fieldpass:*`): it changes additively
//!   only (code `CLAUDE.md` §4). The asset's secret seed is never stored.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::json_rpc::{JsonRpc, RpcError, RpcUrl, RPC_CONNECT_TIMEOUT, RPC_TIMEOUT};
use crate::rate_limit::{ClientKey, FixedWindow, RateWarn, RATE_LIMIT_MAP_CAP, RATE_LIMIT_WINDOW};

/// The Metaplex Core program.
pub const CORE_PROGRAM: &str = "CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d";
/// The System program.
pub const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
/// Devnet's genesis hash (the spike's G1). The mint refuses any other cluster.
pub const DEVNET_GENESIS: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
/// The pass's on-chain name (ODQ-70 (a)).
pub const FIELD_PASS_NAME: &str = "Breakpoint Genesis Field Pass";
/// The pass's metadata, served by nginx from the demo's own origin (ODQ-74 (c)).
pub const FIELD_PASS_URI: &str = "https://demo.mercsandmines.com/assets/field_pass/field_pass.json";
/// The env key for the mint key's 32-byte ed25519 seed, as 64 hex characters. A secret: it
/// lives in a root-only `EnvironmentFile` on the droplet and never reaches a log.
pub const FIELD_PASS_SEED_HEX_KEY: &str = "FIELD_PASS_SEED_HEX";

/// `CreateV2`'s instruction discriminator in Core.
const CREATE_V2: u8 = 20;
/// After the name and uri: plugins `Some([PermanentFreezeDelegate { frozen: true }` with
/// authority `Some(None)`]), then external plugin adapters `Some([])`.
const CREATE_V2_TAIL: [u8; 14] = [1, 1, 0, 0, 0, 5, 1, 1, 0, 1, 0, 0, 0, 0];
/// The instruction's accounts, as indices into the message's keys: asset, collection,
/// authority, payer, owner, update authority, system program, log wrapper. Each optional
/// account is the Core program (index 2), which Core reads as "none".
const CREATE_V2_ACCOUNTS: [u8; 8] = [1, 2, 2, 0, 3, 2, 4, 2];

/// The pass mint's limits, per wallet and per address, and one global ceiling on chain work.
pub const FIELD_PASS_PER_IP: u32 = 30;
pub const FIELD_PASS_PER_WALLET: u32 = 3;
pub const FIELD_PASS_READ_PER_IP: u32 = 120;
pub const FIELD_PASS_READ_PER_WALLET: u32 = 30;
/// Chain work (a mint or a status read, up to four devnet calls each) per window, for
/// everyone. The public devnet allows 10 calls per method per ~10 s from the droplet's one
/// address, shared with the checkout's 4 per 10 s.
pub const FIELD_PASS_CHAIN_PER_WINDOW: u32 = 4;
pub const FIELD_PASS_CHAIN_WINDOW: Duration = Duration::from_secs(10);

// ── The transaction ────────────────────────────────────────────────────────────

/// Solana's compact-u16 length prefix.
pub fn compact_u16(n: u16, out: &mut Vec<u8>) {
    let mut n = n;
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn key32(b58: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let v = bs58::decode(b58).into_vec().expect("a const key is base58");
    out.copy_from_slice(&v);
    out
}

/// A base58 address as 32 bytes; anything else is `None`.
pub fn address(b58: &str) -> Option<[u8; 32]> {
    let v = bs58::decode(b58).into_vec().ok()?;
    <[u8; 32]>::try_from(v.as_slice()).ok()
}

/// `CreateV2`'s data for `name` and `uri`.
pub fn create_v2_data(name: &str, uri: &str) -> Vec<u8> {
    let mut d = vec![CREATE_V2, 0];
    d.extend_from_slice(&(name.len() as u32).to_le_bytes());
    d.extend_from_slice(name.as_bytes());
    d.extend_from_slice(&(uri.len() as u32).to_le_bytes());
    d.extend_from_slice(uri.as_bytes());
    d.extend_from_slice(&CREATE_V2_TAIL);
    d
}

/// The v0 message: header `[2, 0, 3]`, keys `[payer, asset, Core, owner, System]` (writable
/// signers, then read-only unsigned in first use), one `CreateV2`, no lookup tables. An owner
/// equal to any other key is refused, never deduplicated (that would change the header).
pub fn create_v2_message(
    payer: &[u8; 32],
    asset: &[u8; 32],
    owner: &[u8; 32],
    blockhash: &[u8; 32],
    name: &str,
    uri: &str,
) -> Result<Vec<u8>, &'static str> {
    let core = key32(CORE_PROGRAM);
    let system = key32(SYSTEM_PROGRAM);
    if owner == payer || owner == asset || owner == &core || owner == &system || asset == payer {
        return Err("the owner must differ from the payer, the asset and the programs");
    }
    let data = create_v2_data(name, uri);
    let data_len = u16::try_from(data.len()).map_err(|_| "the name and uri are too long")?;
    let mut m = vec![0x80, 2, 0, 3];
    compact_u16(5, &mut m);
    for k in [payer, asset, &core, owner, &system] {
        m.extend_from_slice(k);
    }
    m.extend_from_slice(blockhash);
    compact_u16(1, &mut m);
    m.push(2);
    compact_u16(CREATE_V2_ACCOUNTS.len() as u16, &mut m);
    m.extend_from_slice(&CREATE_V2_ACCOUNTS);
    compact_u16(data_len, &mut m);
    m.extend_from_slice(&data);
    compact_u16(0, &mut m);
    Ok(m)
}

/// The wire bytes: both signatures (payer first), then the message.
pub fn signed_wire(message: &[u8], payer: &SigningKey, asset: &SigningKey) -> (Vec<u8>, [u8; 64]) {
    let first = payer.sign(message).to_bytes();
    let second = asset.sign(message).to_bytes();
    let mut w = Vec::with_capacity(1 + 128 + message.len());
    compact_u16(2, &mut w);
    w.extend_from_slice(&first);
    w.extend_from_slice(&second);
    w.extend_from_slice(message);
    (w, first)
}

/// 64 hex characters as a 32-byte seed; anything else is `None`.
pub fn parse_seed_hex(raw: &str) -> Option<[u8; 32]> {
    if raw.len() != 64 || !raw.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&raw[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

// ── The claim ──────────────────────────────────────────────────────────────────

/// One mint attempt, signed and stored before it is sent. Persisted in Redis: additive only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    /// The wallet that claimed (the asset's owner), base58.
    pub wallet: String,
    /// The asset's address, fixed by its fresh keypair.
    pub asset: String,
    /// The payer's signature, base58: the transaction's id.
    pub signature: String,
    /// The signed wire bytes, base64. A resend sends exactly these.
    pub wire: String,
    /// The blockhash's last valid block height; past it at `finalized`, the bytes can't land.
    pub last_valid_block_height: u64,
}

/// A test seam: awaited inside `InMemoryClaims::claim` before the claim is decided.
pub type PauseHook = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Where claims live: Redis in production, memory in lib tests. Keyed by the Genesis
/// Token's mint, with a `wallet → mint` link for the status read.
#[async_trait]
pub trait Claims: Send + Sync {
    async fn get(&self, mint: &str) -> Result<Option<Attempt>, String>;
    /// Stores `attempt` only if `mint` has no claim (`SET NX`). `Ok(true)`: stored.
    async fn claim(&self, mint: &str, attempt: &Attempt) -> Result<bool, String>;
    /// Replaces the claim with `new` only if the stored claim's signature is still
    /// `old_signature`. `Ok(true)`: replaced.
    async fn replace(&self, mint: &str, old_signature: &str, new: &Attempt) -> Result<bool, String>;
    async fn link(&self, wallet: &str, mint: &str) -> Result<(), String>;
    async fn mint_of(&self, wallet: &str) -> Result<Option<String>, String>;
}

/// The lib tests' twin of `RedisClaims`.
#[derive(Default)]
pub struct InMemoryClaims {
    claims: Mutex<std::collections::HashMap<String, Attempt>>,
    links: Mutex<std::collections::HashMap<String, String>>,
    pause: Option<PauseHook>,
}

impl InMemoryClaims {
    /// A store whose `claim` awaits `hook` first (the race test's seam).
    #[doc(hidden)]
    pub fn with_pause_seam(hook: PauseHook) -> Self {
        Self { pause: Some(hook), ..Self::default() }
    }
}

#[async_trait]
impl Claims for InMemoryClaims {
    async fn get(&self, mint: &str) -> Result<Option<Attempt>, String> {
        Ok(self.claims.lock().unwrap().get(mint).cloned())
    }

    async fn claim(&self, mint: &str, attempt: &Attempt) -> Result<bool, String> {
        if let Some(hook) = &self.pause {
            hook().await;
        }
        let mut claims = self.claims.lock().unwrap();
        if claims.contains_key(mint) {
            return Ok(false);
        }
        claims.insert(mint.to_string(), attempt.clone());
        Ok(true)
    }

    async fn replace(&self, mint: &str, old_signature: &str, new: &Attempt) -> Result<bool, String> {
        let mut claims = self.claims.lock().unwrap();
        match claims.get_mut(mint) {
            Some(stored) if stored.signature == old_signature => {
                *stored = new.clone();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn link(&self, wallet: &str, mint: &str) -> Result<(), String> {
        self.links.lock().unwrap().insert(wallet.to_string(), mint.to_string());
        Ok(())
    }

    async fn mint_of(&self, wallet: &str) -> Result<Option<String>, String> {
        Ok(self.links.lock().unwrap().get(wallet).cloned())
    }
}

/// The Redis claims: `demo:fieldpass:mint:<mint>` holds the attempt's JSON,
/// `demo:fieldpass:wallet:<wallet>` the mint. No TTL: a claim lasts the booth. A demo deploy
/// restarts the server, not Redis, so claims survive it (AOF covers a Redis restart).
pub struct RedisClaims {
    conn: redis::aio::ConnectionManager,
}

impl RedisClaims {
    pub fn new(conn: redis::aio::ConnectionManager) -> Self {
        Self { conn }
    }

    fn mint_key(mint: &str) -> String {
        format!("demo:fieldpass:mint:{mint}")
    }

    fn wallet_key(wallet: &str) -> String {
        format!("demo:fieldpass:wallet:{wallet}")
    }
}

/// Compare-and-set on the attempt's signature (Redis has no CAS; the script is atomic).
const REPLACE_IF_SIGNATURE: &str = r#"
local v = redis.call('GET', KEYS[1])
if v and cjson.decode(v)['signature'] == ARGV[1] then
  redis.call('SET', KEYS[1], ARGV[2])
  return 1
end
return 0
"#;

#[async_trait]
impl Claims for RedisClaims {
    async fn get(&self, mint: &str) -> Result<Option<Attempt>, String> {
        let mut conn = self.conn.clone();
        let raw: Option<String> =
            redis::cmd("GET").arg(Self::mint_key(mint)).query_async(&mut conn).await.map_err(|e| e.to_string())?;
        raw.map(|s| serde_json::from_str(&s).map_err(|e| e.to_string())).transpose()
    }

    async fn claim(&self, mint: &str, attempt: &Attempt) -> Result<bool, String> {
        let mut conn = self.conn.clone();
        let json = serde_json::to_string(attempt).map_err(|e| e.to_string())?;
        let set: Option<String> = redis::cmd("SET")
            .arg(Self::mint_key(mint))
            .arg(json)
            .arg("NX")
            .query_async(&mut conn)
            .await
            .map_err(|e| e.to_string())?;
        Ok(set.is_some())
    }

    async fn replace(&self, mint: &str, old_signature: &str, new: &Attempt) -> Result<bool, String> {
        let mut conn = self.conn.clone();
        let json = serde_json::to_string(new).map_err(|e| e.to_string())?;
        let n: i64 = redis::Script::new(REPLACE_IF_SIGNATURE)
            .key(Self::mint_key(mint))
            .arg(old_signature)
            .arg(json)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| e.to_string())?;
        Ok(n == 1)
    }

    async fn link(&self, wallet: &str, mint: &str) -> Result<(), String> {
        let mut conn = self.conn.clone();
        redis::cmd("SET")
            .arg(Self::wallet_key(wallet))
            .arg(mint)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| e.to_string())
    }

    async fn mint_of(&self, wallet: &str) -> Result<Option<String>, String> {
        let mut conn = self.conn.clone();
        redis::cmd("GET").arg(Self::wallet_key(wallet)).query_async(&mut conn).await.map_err(|e| e.to_string())
    }
}

// ── The minter ─────────────────────────────────────────────────────────────────

/// Where an attempt stands on chain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChainState {
    /// Not landed yet, and it still can (the stored bytes were resent if unseen).
    Pending,
    /// Landed: the asset exists.
    Confirmed,
    /// Proven unable to land: a rebuild may replace it.
    Dead,
}

impl ChainState {
    pub fn as_str(self) -> &'static str {
        match self {
            ChainState::Pending => "pending",
            ChainState::Confirmed => "confirmed",
            ChainState::Dead => "dead",
        }
    }
}

/// Why a mint step failed.
#[derive(Debug)]
pub enum MintError {
    /// The RPC is not devnet: the mint is off until it is.
    WrongCluster,
    /// The RPC failed; `RateLimited` asks the client to retry later.
    Rpc(RpcError),
    /// A reply did not have the expected shape.
    Reply(String),
}

impl From<RpcError> for MintError {
    fn from(e: RpcError) -> Self {
        MintError::Rpc(e)
    }
}

/// The devnet mint client and its key. No `Debug`: it holds the key.
pub struct Minter {
    rpc: JsonRpc,
    payer: SigningKey,
    fenced: tokio::sync::OnceCell<()>,
}

/// The minter `main` sets. `None` (journalled) unless both the devnet RPC URL and a
/// well-formed seed are configured; the pass routes then answer `503` "minting not open".
pub fn from_lookup(get: impl Fn(&str) -> Option<String>, url: Option<&RpcUrl>) -> Option<Arc<Minter>> {
    let raw = get(FIELD_PASS_SEED_HEX_KEY).filter(|v| !v.is_empty());
    let Some(raw) = raw else {
        tracing::info!("field pass mint: off (no {FIELD_PASS_SEED_HEX_KEY})");
        return None;
    };
    let Some(seed) = parse_seed_hex(raw.trim()) else {
        tracing::warn!("{FIELD_PASS_SEED_HEX_KEY} is set but is not 64 hex characters; field pass mint: off");
        return None;
    };
    let Some(url) = url else {
        tracing::warn!("field pass mint: off (no devnet RPC URL)");
        return None;
    };
    match Minter::new(url.clone(), seed, RPC_CONNECT_TIMEOUT, RPC_TIMEOUT) {
        Ok(m) => {
            tracing::info!("field pass mint: on, authority {}", m.authority());
            Some(Arc::new(m))
        }
        Err(e) => {
            tracing::error!("field pass mint: the RPC client failed to build ({e}); off");
            None
        }
    }
}

/// `from_lookup` over the process environment.
pub fn from_env(url: Option<&RpcUrl>) -> Option<Arc<Minter>> {
    from_lookup(|k| std::env::var(k).ok(), url)
}

impl Minter {
    pub fn new(url: RpcUrl, seed: [u8; 32], connect_timeout: Duration, timeout: Duration) -> Result<Self, String> {
        Ok(Self {
            rpc: JsonRpc::new(url, connect_timeout, timeout)?,
            payer: SigningKey::from_bytes(&seed),
            fenced: tokio::sync::OnceCell::new(),
        })
    }

    /// The mint authority and fee payer, base58 (the address the Director funds).
    pub fn authority(&self) -> String {
        bs58::encode(self.payer.verifying_key().to_bytes()).into_string()
    }

    /// Refuses any cluster but devnet. A pass is checked once per process.
    pub async fn fence(&self) -> Result<(), MintError> {
        self.fenced
            .get_or_try_init(|| async {
                let hash = self.rpc.call("getGenesisHash", json!([])).await?;
                if hash.as_str() != Some(DEVNET_GENESIS) {
                    tracing::error!("field pass mint: the devnet RPC URL is not devnet; refusing to mint");
                    return Err(MintError::WrongCluster);
                }
                Ok(())
            })
            .await
            .map(|_| ())
    }

    /// A fresh blockhash and its last valid block height, at `confirmed`.
    pub async fn latest_blockhash(&self) -> Result<([u8; 32], u64), MintError> {
        let r = self.rpc.call("getLatestBlockhash", json!([{"commitment": "confirmed"}])).await?;
        let hash = r["value"]["blockhash"].as_str().and_then(address);
        let last = r["value"]["lastValidBlockHeight"].as_u64();
        match (hash, last) {
            (Some(h), Some(l)) => Ok((h, l)),
            _ => Err(MintError::Reply("getLatestBlockhash: no blockhash".into())),
        }
    }

    /// Builds and signs an attempt for `owner` with a fresh asset key from `asset_seed`.
    pub fn build(&self, owner: &str, blockhash: [u8; 32], last_valid_block_height: u64, asset_seed: [u8; 32]) -> Result<Attempt, String> {
        let owner_key = address(owner).ok_or("the wallet is not a base58 address")?;
        let asset = SigningKey::from_bytes(&asset_seed);
        let asset_pub = asset.verifying_key().to_bytes();
        let message = create_v2_message(
            &self.payer.verifying_key().to_bytes(),
            &asset_pub,
            &owner_key,
            &blockhash,
            FIELD_PASS_NAME,
            FIELD_PASS_URI,
        )?;
        let (wire, sig) = signed_wire(&message, &self.payer, &asset);
        Ok(Attempt {
            wallet: owner.to_string(),
            asset: bs58::encode(asset_pub).into_string(),
            signature: bs58::encode(sig).into_string(),
            wire: STANDARD.encode(wire),
            last_valid_block_height,
        })
    }

    /// Sends the stored bytes, with preflight. Its error never proves "not landed".
    pub async fn send(&self, wire: &str) -> Result<(), RpcError> {
        self.rpc
            .call("sendTransaction", json!([wire, {"encoding": "base64", "preflightCommitment": "confirmed"}]))
            .await
            .map(|_| ())
    }

    /// Where `a` stands. Unseen with its blockhash alive: the stored bytes are resent and it
    /// is `Pending`. `Dead` needs all three reads: the status (history searched) is `null`
    /// or a failure, the `finalized` height is past its last valid one, and the asset
    /// account does not exist.
    pub async fn chain_state(&self, a: &Attempt) -> Result<ChainState, MintError> {
        let r = self
            .rpc
            .call("getSignatureStatuses", json!([[a.signature], {"searchTransactionHistory": true}]))
            .await?;
        let status = &r["value"][0];
        if !status.is_null() {
            let landed = matches!(status["confirmationStatus"].as_str(), Some("confirmed" | "finalized"));
            if !landed {
                return Ok(ChainState::Pending);
            }
            if status["err"].is_null() {
                return Ok(ChainState::Confirmed);
            }
        }
        let height = self.rpc.call("getBlockHeight", json!([{"commitment": "finalized"}])).await?;
        let height = height.as_u64().ok_or_else(|| MintError::Reply("getBlockHeight: not a number".into()))?;
        if status.is_null() && height <= a.last_valid_block_height {
            if let Err(e) = self.send(&a.wire).await {
                tracing::info!("field pass resend: {e}");
            }
            return Ok(ChainState::Pending);
        }
        let account = self
            .rpc
            .call("getAccountInfo", json!([a.asset, {"commitment": "finalized", "encoding": "base64"}]))
            .await?;
        if account["value"].is_null() {
            Ok(ChainState::Dead)
        } else {
            Ok(ChainState::Confirmed)
        }
    }
}

// ── The routes' state ──────────────────────────────────────────────────────────

/// The pass routes' limiters.
pub struct Limits {
    pub ip: FixedWindow<ClientKey>,
    pub wallet: FixedWindow<String>,
    pub read_ip: FixedWindow<ClientKey>,
    pub read_wallet: FixedWindow<String>,
    pub chain: FixedWindow<()>,
    /// Journals refusals at the chain ceiling: the first, then at most hourly.
    pub chain_warn: RateWarn,
}

impl Default for Limits {
    fn default() -> Self {
        fn w<K: Eq + std::hash::Hash + Clone>(name: &'static str, limit: u32) -> FixedWindow<K> {
            FixedWindow::new(name, limit, RATE_LIMIT_WINDOW, RATE_LIMIT_MAP_CAP)
        }
        Self {
            ip: w("field_pass_ip", FIELD_PASS_PER_IP),
            wallet: w("field_pass_wallet", FIELD_PASS_PER_WALLET),
            read_ip: w("field_pass_read_ip", FIELD_PASS_READ_PER_IP),
            read_wallet: w("field_pass_read_wallet", FIELD_PASS_READ_PER_WALLET),
            chain: FixedWindow::new("field_pass_chain", FIELD_PASS_CHAIN_PER_WINDOW, FIELD_PASS_CHAIN_WINDOW, RATE_LIMIT_MAP_CAP),
            chain_warn: RateWarn::new(),
        }
    }
}

/// Everything the pass routes hold (`AppState::field_pass`). `main` sets the minter and the
/// Redis claims; `AppState::new` starts with no minter and in-memory claims.
pub struct FieldPassState {
    pub minter: Option<Arc<Minter>>,
    pub claims: Arc<dyn Claims>,
    pub limits: Limits,
}

impl Default for FieldPassState {
    fn default() -> Self {
        Self { minter: None, claims: Arc::new(InMemoryClaims::default()), limits: Limits::default() }
    }
}

/// The client's view of an attempt.
pub fn view(a: &Attempt, state: ChainState) -> Value {
    json!({"asset": a.asset, "signature": a.signature, "state": state.as_str(), "cluster": "devnet"})
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, VerifyingKey};

    /// The ODQ-70 spike's Seeker mint `kxb9…`, as `getTransaction` (base64, finalized) returned
    /// it (`Raw/bp7_spike_tx_kxb9.json`, sha256 `302ef92c…`).
    const SPIKE_KXB9: &str = "AiXod7wLzJ6FBaXyUGkCsapqKReZMF2fyLNIiUEBBeDv6KqxIe4sM61d4X6MLv+6owDzZ0ghvFWo2qzlzS/gRw4O7Bw/xkSLLqCPCutZ7XizlK8bmDS9/qG1TKrHnTlKBkgH8+I7CwHDs/Bp9PadMvyVitPTpyGIirdyxFaLoO8OgAIAAwUB+kVxx4ojdvHPb8vXT2kFroqL44HjB6l7RrvMvbo4RRrB/fkEFZfzFLLAqxtWy+8dbKJvp5xHma9B28+WQ2T4r1SrEL2XpUKgnvezmIndDNOUpMzp36bNyX6+LSNbp0icghdf1uc5y990oRbpvnXDkulEllcAj+4FY9Xao8dh3AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAjmY066PDYalAnplLiTF0E0nqbQY13slQAiXzDTGk4P4BAggBAgIAAwIEAsYBFAAbAAAAVHJhc2ggS2hhbiwgQnJlYWtwb2ludCBUZXN0kwAAAGh0dHBzOi8vZ2lzdC5naXRodWJ1c2VyY29udGVudC5jb20vU29sb1hhbmRlclNPTC9hODI5YWZiMDY5NGFkNWFjYmMwOTVlNzk5ODlhZWY5Mi9yYXcvM2YyMDcyZWRjZDg2YzNkZjkyZjdiYjkwODc1MTdiYWM5ZGUyNjY3My90cmFzaF9raGFuX3Rlc3QuanNvbgEBAAAABQEBAAEAAAAAAA==";
    /// The spike's second-key mint `3d6e…` (`Raw/bp7_spike_tx_3d6e.json`, sha256 `5a9f5614…`).
    const SPIKE_3D6E: &str = "AoMpscUmUTixFuQeEDIePoRqwmMqxzW7oVpO3Npum1LNd7EFqtRpUYnv+sJsxaAdMjrwcuZH1hRvV73OrSCrVw0R7fB0YK3KcHd0meweJeVnsFMjWNuLtCYObjG2jibIG1QeYYgBuf0K3vqOmMZYjUt8Ds+1Uj7JqivMhR+hphcFgAIAAwUB+kVxx4ojdvHPb8vXT2kFroqL44HjB6l7RrvMvbo4RU8ukYtEvFhZ6oZKrL0rgCku55IiDsAr1ZAh7H2Kvp3Tr1SrEL2XpUKgnvezmIndDNOUpMzp36bNyX6+LSNbp0gHXzMJTr5tTM1tvIKQlC4ScR47MWqnzMTv67zWu2peqAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAVDn5W04/X7NlGF0/F/Pk0uUAmeli3kNrcFgBCxJ237UBAggBAgIAAwIEAsYBFAAbAAAAVHJhc2ggS2hhbiwgQnJlYWtwb2ludCBUZXN0kwAAAGh0dHBzOi8vZ2lzdC5naXRodWJ1c2VyY29udGVudC5jb20vU29sb1hhbmRlclNPTC9hODI5YWZiMDY5NGFkNWFjYmMwOTVlNzk5ODlhZWY5Mi9yYXcvM2YyMDcyZWRjZDg2YzNkZjkyZjdiYjkwODc1MTdiYWM5ZGUyNjY3My90cmFzaF9raGFuX3Rlc3QuanNvbgEBAAAABQEBAAEAAAAAAA==";
    const SPIKE_NAME: &str = "Trash Khan, Breakpoint Test";
    const SPIKE_URI: &str = "https://gist.githubusercontent.com/SoloXanderSOL/a829afb0694ad5acbc095e79989aef92/raw/3f2072edcd86c3df92f7bb9087517bac9de26673/trash_khan_test.json";
    const SPIKE_PAYER: &str = "8ikSDZzhPhPyQUPD3yaDdXa6jj4mKy5Kt6Cg2BaQEHi";

    /// mpl-core 1.10.0's own `create()` for this module's name and uri, offline: payer seed
    /// `[1; 32]`, asset seed `[2; 32]`, owner the key of seed `[3; 32]`, blockhash `[7; 32]`
    /// (Wiki's `oracle.mjs`, umi 1.6.0, Node 18.19.1; 456 bytes, message 327).
    const LIBRARY_FIELD_PASS: &str = "AlMsFmsv7jQZGQd1UmKfr4ZZY8UF25XT5BlBueOOj46EFZkGeJe0PqByGjRMqY0Mq8V9J+5FF9178ejiVyy0mgNJ34nGAN8ku184Tm1eviFgt/qc/9O3iwaI7eOhf3IYclOlhLzUhySCoMbFKVU5W68LirsbjH24i28vv7HWYG8PgAIAAwWKiOPddAnxlf1S2y08ul1yymcJvx2UEhvzdIgBtA9vXIE5dw6ofRdfVqNUZsNMfszLjYqRtO43ol32D1uPybOUr1SrEL2XpUKgnvezmIndDNOUpMzp36bNyX6+LSNbp0jtSSjGKNHCxurpAziQWZVhKVknOlxj+TY2wUYUrIc30QAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcBAggBAgIAAwIEAnUUAB0AAABCcmVha3BvaW50IEdlbmVzaXMgRmllbGQgUGFzc0AAAABodHRwczovL2RlbW8ubWVyY3NhbmRtaW5lcy5jb20vYXNzZXRzL2ZpZWxkX3Bhc3MvZmllbGRfcGFzcy5qc29uAQEAAAAFAQEAAQAAAAAA";

    fn k(b58: &str) -> [u8; 32] {
        address(b58).expect("a base58 key")
    }

    /// Rebuilds a spike message from its asset, owner and blockhash, and checks it byte for
    /// byte, then both signatures over the rebuilt bytes.
    fn spike_matches(wire_b64: &str, asset: &str, owner: &str, blockhash: &str, id: &str) {
        let wire = STANDARD.decode(wire_b64).expect("base64");
        assert_eq!(wire.len(), 538);
        assert_eq!(wire[0], 2, "two signatures");
        let built = create_v2_message(&k(SPIKE_PAYER), &k(asset), &k(owner), &k(blockhash), SPIKE_NAME, SPIKE_URI)
            .expect("the spike's keys are distinct");
        assert_eq!(built.len(), 409);
        assert_eq!(&wire[129..], built.as_slice(), "the message, byte for byte");
        for (i, signer) in [SPIKE_PAYER, asset].into_iter().enumerate() {
            let sig = Signature::from_bytes(wire[1 + 64 * i..65 + 64 * i].try_into().unwrap());
            VerifyingKey::from_bytes(&k(signer)).unwrap().verify_strict(&built, &sig).expect("the spike's signature verifies");
        }
        assert_eq!(bs58::encode(&wire[1..65]).into_string(), id, "the payer's signature is the id");
    }

    #[test]
    fn field_pass_rebuilds_the_spike_seeker_mint() {
        spike_matches(
            SPIKE_KXB9,
            "2oTAjDVgZ6JBqZNt21ATQWR3yTrH2MkYZa9yYuTyLt7y",
            "BXwewwFQxTREU98Yta7NFXiogMMZg1CkiQk9DKrb8dij",
            "AasHYVc81sBcYctUuGvQEjGbb8W16xPFBehaQcihGhfB",
            "kxb9CLdZhWaD9NEGDxZiTpk95RiNJnZh5iTUpxFfKtPXJrHhLm7WatwkZWNwofu52HfioJhfdDq6fYDwxu1DrJq",
        );
    }

    #[test]
    fn field_pass_rebuilds_the_spike_second_mint() {
        spike_matches(
            SPIKE_3D6E,
            "6L6PpxuDCceXhY5LbM6EZZxoBJh6QJbidYhrTPYpXWKk",
            "Vn3pmN7AHYuLGsYfeL7zFmvmvp5dkSr6FjDFUpTugUo",
            "6fnX7SQZ346Wt9bLfPVUeuNFPfbyekKmUagkeZcrMk5r",
            "3d6e3j6WakXt1zSyq6xXSJFwCWC7U26ZFUGfuAQQo3wiZvJUgUanbQTBWnb19TyJcR8g6SCMCjHr2RicNhpbh79e",
        );
    }

    /// The pass's own name and uri (a 1-byte data length, which the spike never used), signed
    /// with fixed seeds: the whole wire equals mpl-core's.
    #[test]
    fn field_pass_build_matches_the_library() {
        let url = RpcUrl::parse("http://127.0.0.1:9/").unwrap();
        let minter = Minter::new(url, [1u8; 32], RPC_CONNECT_TIMEOUT, RPC_TIMEOUT).unwrap();
        let owner = bs58::encode(SigningKey::from_bytes(&[3u8; 32]).verifying_key().to_bytes()).into_string();
        let a = minter.build(&owner, [7u8; 32], 1000, [2u8; 32]).expect("builds");
        assert_eq!(a.wire, LIBRARY_FIELD_PASS);
        assert_eq!(a.wallet, owner);
        assert_eq!(a.asset, "9hSR6S7WPtxmTojgo6GG3k4yDPecgJY292j7xrsUGWBu");
        assert_eq!(a.last_valid_block_height, 1000);
        let wire = STANDARD.decode(&a.wire).unwrap();
        assert_eq!(a.signature, bs58::encode(&wire[1..65]).into_string());
        assert_eq!(minter.authority(), "AKnL4NNf3DGWZJS6cPknBuEGnVsV4A4m5tgebLHaRSZ9");
    }

    #[test]
    fn field_pass_compact_u16_table() {
        let table: [(u16, &[u8]); 7] = [
            (0, &[0x00]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (198, &[0xc6, 0x01]),
            (16383, &[0xff, 0x7f]),
            (16384, &[0x80, 0x80, 0x01]),
            (65535, &[0xff, 0xff, 0x03]),
        ];
        for (n, want) in table {
            let mut out = Vec::new();
            compact_u16(n, &mut out);
            assert_eq!(out, want, "{n}");
        }
    }

    /// An owner equal to the payer, the Core program or the System program is refused, not
    /// deduplicated; so is an asset equal to the payer.
    #[test]
    fn field_pass_refuses_a_shared_key() {
        let (payer, asset, owner, hash) = ([1u8; 32], [2u8; 32], [3u8; 32], [7u8; 32]);
        assert!(create_v2_message(&payer, &asset, &owner, &hash, "n", "u").is_ok());
        for bad in [payer, asset, k(CORE_PROGRAM), k(SYSTEM_PROGRAM)] {
            assert!(create_v2_message(&payer, &asset, &bad, &hash, "n", "u").is_err());
        }
        assert!(create_v2_message(&payer, &payer, &owner, &hash, "n", "u").is_err());
    }

    #[test]
    fn field_pass_seed_parsing() {
        let hex: String = (0..32).map(|i| format!("{:02x}", i * 7 % 256)).collect();
        let seed = parse_seed_hex(&hex).expect("64 hex characters");
        assert_eq!(seed[1], 7);
        assert_eq!(parse_seed_hex(&hex.to_uppercase()), Some(seed));
        assert_eq!(parse_seed_hex(&hex[..62]), None);
        assert_eq!(parse_seed_hex(&format!("{}zz", &hex[..62])), None);
        assert_eq!(parse_seed_hex(""), None);
        let url = RpcUrl::parse("http://127.0.0.1:9/").unwrap();
        let get = |v: &'static str| move |key: &str| (key == FIELD_PASS_SEED_HEX_KEY).then(|| v.to_string());
        assert!(from_lookup(get(""), Some(&url)).is_none(), "empty is off");
        assert!(from_lookup(get("abc"), Some(&url)).is_none(), "malformed is off");
        let good: &'static str = Box::leak(hex.into_boxed_str());
        assert!(from_lookup(get(good), None).is_none(), "no devnet URL is off");
        assert!(from_lookup(get(good), Some(&url)).is_some(), "a seed and a URL is on");
    }

    fn attempt(wallet: &str, sig: &str) -> Attempt {
        Attempt { wallet: wallet.into(), asset: "A".into(), signature: sig.into(), wire: "W".into(), last_valid_block_height: 9 }
    }

    /// The twin's NX and compare-and-set, which the route tests rely on.
    #[tokio::test]
    async fn field_pass_in_memory_claims_are_nx_and_cas() {
        let c = InMemoryClaims::default();
        assert!(c.claim("m", &attempt("w1", "s1")).await.unwrap());
        assert!(!c.claim("m", &attempt("w2", "s2")).await.unwrap(), "a second claim loses");
        assert_eq!(c.get("m").await.unwrap(), Some(attempt("w1", "s1")));
        assert!(!c.replace("m", "s9", &attempt("w1", "s3")).await.unwrap(), "a stale signature loses");
        assert!(c.replace("m", "s1", &attempt("w1", "s3")).await.unwrap());
        assert_eq!(c.get("m").await.unwrap(), Some(attempt("w1", "s3")));
        assert!(!c.replace("none", "s1", &attempt("w1", "s4")).await.unwrap(), "no claim, no replace");
        assert_eq!(c.mint_of("w1").await.unwrap(), None);
        c.link("w1", "m").await.unwrap();
        assert_eq!(c.mint_of("w1").await.unwrap(), Some("m".into()));
    }

    /// The JSON shape Redis stores (additive only from the first deploy).
    #[test]
    fn field_pass_attempt_json_keys() {
        let v = serde_json::to_value(attempt("w", "s")).unwrap();
        let keys: std::collections::BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["asset", "last_valid_block_height", "signature", "wallet", "wire"].into_iter().collect());
    }
}
