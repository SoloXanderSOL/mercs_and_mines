mod api_types;
mod routes;

#[tokio::main]
async fn main() {
    let app = routes::router();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("Failed to bind 0.0.0.0:3000");
    println!("Mercs and Mines server listening on 0.0.0.0:3000");
    axum::serve(listener, app).await.expect("Server error");
}
