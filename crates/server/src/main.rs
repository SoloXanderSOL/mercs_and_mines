use std::sync::Arc;
use mercs_server::config::Config;
use mercs_server::state::AppState;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let config = Arc::new(Config::from_env());
    let bind_addr = config.server.bind_addr.clone();
    let log_dir = std::env::var("LOG_DIR").unwrap_or_else(|_| "./logs".into());
    let state = Arc::new(AppState::new(std::path::PathBuf::from(log_dir), config));
    let app = mercs_server::routes::router(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|_| panic!("Failed to bind {}", bind_addr));
    println!("Mercs and Mines server listening on {}", bind_addr);
    axum::serve(listener, app).await.expect("Server error");
}
