use std::sync::Arc;

use axum::{
    extract::{
        ws::{rejection::WebSocketUpgradeRejection, Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::auth::AuthSession;
use crate::convoy_expiry::ConvoyEvent;
use crate::repository::CampaignLifecycle;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/campaign/:campaign_id/events/ws", get(convoy_events_ws_handler))
}

/// A member's campaign event stream (GAP-74, 5a-3b). Order: `401` (no session) → `404`
/// (unknown campaign) → `403` (not a member) → `409` (not `Active` or `Ending`) → upgrade.
/// `Ending` is admitted: convoys dispatched while `Active` still land, and §2a's `Ending` is
/// where players receive notifications. Every refusal is an HTTP answer before the upgrade.
///
/// - `AuthSession` reads the `Authorization` header, which a browser `WebSocket` can't
///   send. The browser carrier belongs to GAP-75 (5b-2), with the socket's first `app/` user.
/// - The upgrade is taken as a `Result` so these checks run, and can be tested, before
///   axum's own upgrade rejection (in a `oneshot` test an admitted request answers `426`).
pub async fn convoy_events_ws_handler(
    AuthSession(session): AuthSession,
    State(state): State<Arc<AppState>>,
    Path(campaign_id): Path<Uuid>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let wallet: Vec<u8> = match bs58::decode(&session.wallet_address).into_vec() {
        Ok(w) => w,
        Err(_) => return super::unauthorized("session wallet is not valid base58").into_response(),
    };
    let campaign = match state.campaign_repo.get_campaign(campaign_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return super::not_found("campaign not found").into_response(),
        Err(e) => {
            return super::internal_error(format!("events socket: campaign read failed for {campaign_id}: {e}")).into_response();
        }
    };
    match state.membership_repo.get_membership(&wallet, campaign_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return super::forbidden("not a member of this campaign").into_response(),
        Err(e) => {
            return super::internal_error(format!("events socket: membership read failed for {campaign_id}: {e}")).into_response();
        }
    }
    if !matches!(campaign.state, CampaignLifecycle::Active | CampaignLifecycle::Ending) {
        return super::conflict("campaign is not active").into_response();
    }

    let ws = match ws {
        Ok(w) => w,
        Err(r) => return r.into_response(),
    };
    ws.on_upgrade(move |socket| handle_convoy_ws(socket, state, campaign_id))
        .into_response()
}

async fn handle_convoy_ws(mut socket: WebSocket, state: Arc<AppState>, campaign_id: Uuid) {
    let (tx, mut rx) = mpsc::unbounded_channel::<ConvoyEvent>();
    state.convoy_event_senders.insert(campaign_id, tx);

    while let Some(event) = rx.recv().await {
        let json = match serde_json::to_string(&event) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to serialize ConvoyEvent: {:?}", e);
                continue;
            }
        };
        if socket.send(Message::Text(json)).await.is_err() {
            break; // client disconnected
        }
    }

    state.convoy_event_senders.remove(&campaign_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::repository::NewCampaignInstance;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    const MEMBER: [u8; 32] = [8u8; 32];

    /// A state with a signed-in session `"t"` for `MEMBER`.
    fn signed_in() -> AppState {
        let state = AppState::new(Arc::new(Config::default()));
        let wallet = bs58::encode(MEMBER).into_string();
        let now = chrono::Utc::now().timestamp();
        state.sessions.insert("t".into(), shared::SessionToken {
            token_id: "t".into(),
            account_id: wallet.clone(),
            wallet_address: wallet,
            issued_at: now,
            expires_at: now + 600,
        });
        state
    }

    /// A `Pending` campaign, optionally activated, optionally with `MEMBER` in it.
    async fn campaign(state: &AppState, active: bool, member: bool) -> Uuid {
        let id = state.campaign_repo
            .create_campaign(&NewCampaignInstance { sector_id: Uuid::new_v4(), map_seed: 1 })
            .await
            .expect("create_campaign")
            .campaign_id;
        if active {
            let ok = state.campaign_repo
                .activate_campaign(id, chrono::Utc::now() + chrono::Duration::days(90), serde_json::json!({}))
                .await
                .expect("activate_campaign");
            assert!(ok, "the fixture campaign must activate");
        }
        if member {
            state.membership_repo.add_member(&MEMBER, id, None, None).await.expect("add_member");
        }
        id
    }

    /// A GET with every WebSocket upgrade header, so a `426` means every check passed.
    async fn open(state: &Arc<AppState>, campaign_id: Uuid, token: Option<&str>) -> StatusCode {
        let mut req = Request::builder()
            .method("GET")
            .uri(format!("/api/campaign/{campaign_id}/events/ws"))
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        router().with_state(Arc::clone(state)).oneshot(req.body(Body::empty()).unwrap()).await.unwrap().status()
    }

    #[tokio::test]
    async fn events_ws_requires_a_session() {
        let state = signed_in();
        let id = campaign(&state, true, true).await;
        assert_eq!(open(&Arc::new(state), id, None).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn events_ws_unknown_campaign_is_404() {
        let state = Arc::new(signed_in());
        assert_eq!(open(&state, Uuid::new_v4(), Some("t")).await, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn events_ws_refuses_a_non_member() {
        let state = signed_in();
        let id = campaign(&state, true, false).await;
        assert_eq!(open(&Arc::new(state), id, Some("t")).await, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn events_ws_refuses_pending_and_ended() {
        let state = signed_in();
        let id = campaign(&state, false, true).await;
        let state = Arc::new(state);
        assert_eq!(open(&state, id, Some("t")).await, StatusCode::CONFLICT, "Pending");
        state.campaign_repo.update_sector_state(id, CampaignLifecycle::Ended).await.unwrap();
        assert_eq!(open(&state, id, Some("t")).await, StatusCode::CONFLICT, "Ended");
    }

    #[tokio::test]
    async fn events_ws_admits_active_and_ending() {
        let state = signed_in();
        let id = campaign(&state, true, true).await;
        let state = Arc::new(state);
        assert_eq!(open(&state, id, Some("t")).await, StatusCode::UPGRADE_REQUIRED, "Active");
        state.campaign_repo.update_sector_state(id, CampaignLifecycle::Ending).await.unwrap();
        assert_eq!(open(&state, id, Some("t")).await, StatusCode::UPGRADE_REQUIRED, "Ending");
    }
}
