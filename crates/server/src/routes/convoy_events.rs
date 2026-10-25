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
use crate::state::{AppState, CONVOY_EVENT_CHANNEL};

/// Largest client frame and message the convoy events socket accepts (1 KiB). The
/// socket has no inbound protocol: frames are drained and ignored.
const CONVOY_WS_MAX_MESSAGE: usize = 1024;

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
    ws.max_message_size(CONVOY_WS_MAX_MESSAGE)
        .max_frame_size(CONVOY_WS_MAX_MESSAGE)
        .on_upgrade(move |socket| handle_convoy_ws(socket, state, campaign_id))
        .into_response()
}

async fn handle_convoy_ws(mut socket: WebSocket, state: Arc<AppState>, campaign_id: Uuid) {
    let (tx, mut rx) = mpsc::channel::<ConvoyEvent>(CONVOY_EVENT_CHANNEL);
    // Hold only a weak handle. When the producer drops a full sender, or a newer
    // socket for this campaign replaces it, `rx` ends and this handler closes.
    let weak = tx.downgrade();
    state.convoy_event_senders.insert(campaign_id, tx);

    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Some(event) => {
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
                None => break,
            },
            msg = socket.recv() => match msg {
                // The client is gone, or sent a frame over the cap.
                None | Some(Err(_)) => break,
                // Drained and ignored. A Close is answered on the next read, so keep reading.
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = socket.close().await;
    // Remove our own sender only: a newer socket may have replaced it (GAP-75).
    let ours = weak.upgrade();
    state
        .convoy_event_senders
        .remove_if(&campaign_id, |_, s| ours.as_ref().is_some_and(|o| o.same_channel(s)));
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

    type Client = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;
    type ClientMsg = tokio_tungstenite::tungstenite::Message;

    /// A live events socket for `MEMBER` (token `"t"`) on campaign `id`.
    async fn connect(addr: std::net::SocketAddr, id: Uuid) -> Client {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://{addr}/api/campaign/{id}/events/ws")
            .into_client_request()
            .expect("request");
        req.headers_mut().insert("authorization", "Bearer t".parse().unwrap());
        let (ws, _) = tokio_tungstenite::connect_async(req).await.expect("connect");
        ws
    }

    /// Polls (10 ms steps, 2 s at most) until the campaign's sender is present or absent.
    async fn wait_for_sender(state: &AppState, id: Uuid, present: bool) {
        let poll = async {
            while state.convoy_event_senders.contains_key(&id) != present {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), poll)
            .await
            .unwrap_or_else(|_| panic!("the sender never became present = {present}"));
    }

    /// The next client message, within 2 s.
    async fn next_msg(ws: &mut Client) -> Option<Result<ClientMsg, tokio_tungstenite::tungstenite::Error>> {
        use futures_util::StreamExt;
        tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await.expect("timed out")
    }

    /// The socket closes: a `Close(None)`, then the end of the stream.
    async fn assert_closes(ws: &mut Client) {
        let msg = next_msg(ws).await.expect("stream ended").expect("read error");
        assert!(matches!(msg, ClientMsg::Close(None)), "expected Close(None), got {msg:?}");
        assert!(matches!(next_msg(ws).await, None | Some(Err(_))), "the socket must end after its Close");
    }

    /// A live server over `state` with an `Active` campaign `MEMBER` belongs to.
    async fn live() -> (Arc<AppState>, Uuid, std::net::SocketAddr) {
        let state = signed_in();
        let id = campaign(&state, true, true).await;
        let state = Arc::new(state);
        let addr = crate::routes::serve_on_loopback(router().with_state(Arc::clone(&state))).await;
        (state, id, addr)
    }

    #[tokio::test]
    async fn events_ws_client_close_ends_the_handler_and_removes_its_sender() {
        use futures_util::SinkExt;
        let (state, id, addr) = live().await;
        let mut ws = connect(addr, id).await;
        wait_for_sender(&state, id, true).await;
        ws.send(ClientMsg::Close(None)).await.expect("send Close");
        assert_closes(&mut ws).await;
        wait_for_sender(&state, id, false).await;
    }

    #[tokio::test]
    async fn events_ws_replaced_socket_leaves_the_new_sender() {
        use futures_util::StreamExt;
        let (state, id, addr) = live().await;
        let mut a = connect(addr, id).await;
        wait_for_sender(&state, id, true).await;
        let mut b = connect(addr, id).await;
        // A closes only once B's insert has replaced A's sender.
        assert_closes(&mut a).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(500), b.next()).await.is_err(),
            "B must get nothing: A's exit must not take B's sender"
        );
        let live = state.convoy_event_senders.get(&id).map(|s| !s.is_closed());
        assert_eq!(live, Some(true), "the map keeps B's live sender");
    }

    #[tokio::test]
    async fn events_ws_over_cap_frame_ends_the_socket() {
        use futures_util::SinkExt;
        let (state, id, addr) = live().await;
        let mut ws = connect(addr, id).await;
        wait_for_sender(&state, id, true).await;
        ws.send(ClientMsg::Text("x".repeat(1025))).await.expect("send");
        assert_closes(&mut ws).await;
        wait_for_sender(&state, id, false).await;
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
