mod api_types;
mod log_writer;
mod routes;
mod state;

use std::sync::Arc;
use state::AppState;

#[tokio::main]
async fn main() {
    let log_dir = std::env::var("LOG_DIR")
        .unwrap_or_else(|_| "./logs".into());
    let state = Arc::new(AppState::new(std::path::PathBuf::from(log_dir)));
    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("Failed to bind 0.0.0.0:3000");
    println!("Mercs and Mines server listening on 0.0.0.0:3000");
    axum::serve(listener, app).await.expect("Server error");
}
