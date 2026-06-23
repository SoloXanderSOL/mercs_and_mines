use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::convoy_expiry::ConvoyEvent;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/campaign/:campaign_id/events/ws", get(convoy_events_ws_handler))
}

pub async fn convoy_events_ws_handler(
    State(state): State<Arc<AppState>>,
    Path(campaign_id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> Response {
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
