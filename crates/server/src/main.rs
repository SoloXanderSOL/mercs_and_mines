mod api_types;
mod routes;
mod state;

use std::sync::Arc;
use state::AppState;

#[tokio::main]
async fn main() {
    let state = Arc::new(AppState::new());
    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("Failed to bind 0.0.0.0:3000");
    println!("Mercs and Mines server listening on 0.0.0.0:3000");
    axum::serve(listener, app).await.expect("Server error");
}
