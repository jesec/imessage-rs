use axum::Json;
/// Webhook routes:
///   GET /api/v1/webhook
use axum::extract::{Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::middleware::error::{AppError, success_response_with_message};
use crate::state::AppState;

#[derive(Debug, Deserialize, Default)]
pub struct WebhookQueryParams {
    pub url: Option<String>,
}

/// GET /api/v1/webhook — list webhook targets loaded from config.
pub async fn get_webhooks(
    State(state): State<AppState>,
    Query(params): Query<WebhookQueryParams>,
) -> Result<Json<Value>, AppError> {
    let Some(ref ws) = state.webhook_service else {
        return Ok(Json(success_response_with_message(
            "Successfully fetched webhooks!",
            json!([]),
        )));
    };

    let targets = ws.get_targets().await;

    let data: Vec<Value> = targets
        .iter()
        .filter(|wh| {
            if let Some(ref url) = params.url {
                return wh.url == *url;
            }
            true
        })
        .map(|wh| {
            json!({
                "url": wh.url,
                "events": wh.events,
            })
        })
        .collect();

    Ok(Json(success_response_with_message(
        "Successfully fetched webhooks!",
        json!(data),
    )))
}
