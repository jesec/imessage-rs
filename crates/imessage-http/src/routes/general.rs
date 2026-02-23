/// General routes: GET /api/v1/ping
use axum::Json;
use serde_json::{Value, json};

use crate::middleware::error::success_response_with_message;

/// GET /api/v1/ping
pub async fn ping() -> Json<Value> {
    Json(success_response_with_message(
        "Ping received!",
        json!("pong"),
    ))
}
