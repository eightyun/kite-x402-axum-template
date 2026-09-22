//! Local upstream for trying the x402 wrapper without an external API.
//!
//! Run with `cargo run --example mock_upstream`, then point UPSTREAM_URL at
//! http://127.0.0.1:3001. The wrapper maps /v1/price to /price.

use axum::{http::StatusCode, routing::get, Json, Router};
use serde_json::json;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let app = Router::new()
        .route("/price", get(|| async { Json(json!({"price": 123})) }))
        .route(
            "/fail",
            get(|| async {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "upstream failed"})),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3001").await?;
    println!("mock upstream listening on http://127.0.0.1:3001");
    axum::serve(listener, app).await
}
