//! The demo's checkout rail (BP-5, **demo-only**): devnet, test USDC, and a devnet-only
//! recipient whose key never leaves the build machine. The `dev` port replaces this file
//! with config (cluster, recipient and mint from `ServerConfig`); nothing else names them.

use std::sync::Arc;

use crate::json_rpc::{JsonRpc, RpcUrl, RPC_CONNECT_TIMEOUT, RPC_TIMEOUT};
use crate::payment::PaymentRail;
use crate::solana_pay::SolanaPayRail;

pub const DEMO_CLUSTER: &str = "devnet";
/// Circle's devnet USDC (legacy SPL Token, 6 decimals; measured at BP-5's spec check).
pub const DEMO_USDC_MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
/// The devnet treasury stand-in (Director, 2026-10-27).
pub const DEMO_RECIPIENT: &str = "Wo2mG8KsRGCdbcGyYV89HoS6wwLuUR2n8HeeE8zY3ow";

/// The rail `AppState` holds. `None` when no devnet RPC URL is configured, or when the
/// client fails to build (journalled); the checkout routes then answer `503`.
pub fn rail(url: Option<&RpcUrl>) -> Option<Arc<dyn PaymentRail>> {
    let url = url?.clone();
    match JsonRpc::new(url, RPC_CONNECT_TIMEOUT, RPC_TIMEOUT) {
        Ok(rpc) => {
            tracing::info!("checkout: on ({DEMO_CLUSTER})");
            Some(Arc::new(SolanaPayRail::new(rpc, DEMO_RECIPIENT, DEMO_USDC_MINT, DEMO_CLUSTER)))
        }
        Err(e) => {
            tracing::error!("checkout: the RPC client failed to build ({e}); the checkout routes answer 503");
            None
        }
    }
}
