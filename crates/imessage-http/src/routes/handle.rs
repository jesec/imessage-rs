use axum::Json;
/// Handle routes:
///   GET  /api/v1/handle/count
///   POST /api/v1/handle/query
///   GET  /api/v1/handle/:guid
///   GET  /api/v1/handle/:guid/focus            [Private API]
///   POST /api/v1/handle/availability/imessage   [Private API]
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use imessage_db::imessage::types::{ChatQueryParams, HandleQueryParams};
use imessage_private_api::actions;
use imessage_serializers::chat::serialize_chat;
use imessage_serializers::config::ChatSerializerConfig;
use imessage_serializers::handle::{serialize_handle, serialize_handles};

use crate::extractors::AppJson;
use crate::middleware::error::{AppError, success_response, success_response_with_metadata};
use crate::state::AppState;

/// Handle count query params
#[derive(Debug, Deserialize, Default)]
pub struct HandleCountParams {
    pub address: Option<String>,
}

/// GET /api/v1/handle/count
pub async fn count(
    State(state): State<AppState>,
    Query(params): Query<HandleCountParams>,
) -> Result<Json<Value>, AppError> {
    let total = state
        .imessage_repo
        .lock()
        .get_handle_count(params.address.as_deref())
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    Ok(Json(success_response(json!({ "total": total }))))
}

/// POST /api/v1/handle/query body
#[derive(Debug, Deserialize, Default)]
pub struct HandleQueryBody {
    pub address: Option<String>,
    #[serde(rename = "with")]
    pub with_query: Option<Vec<String>>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

/// POST /api/v1/handle/query
pub async fn query(
    State(state): State<AppState>,
    AppJson(body): AppJson<HandleQueryBody>,
) -> Result<Json<Value>, AppError> {
    let offset = body.offset.unwrap_or(0);
    let limit = body.limit.unwrap_or(100).min(1000); // max:1000

    let (handles, total) = state
        .imessage_repo
        .lock()
        .get_handles(&HandleQueryParams {
            address: body.address.clone(),
            limit,
            offset,
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let mut results = serialize_handles(&handles, false);

    // Enrich with chats if requested via `with` query
    let with_query: Vec<String> = body
        .with_query
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|s| s.trim().to_lowercase())
        .collect();
    let with_chats = with_query.iter().any(|s| s == "chat" || s == "chats");
    let with_chat_participants = with_query
        .iter()
        .any(|s| s == "chat.participants" || s == "chats.participants");

    if with_chats || with_chat_participants {
        // Fetch all chats with participants
        let (chats, _) = state
            .imessage_repo
            .lock()
            .get_chats(&ChatQueryParams {
                with_participants: true,
                ..Default::default()
            })
            .map_err(|e| AppError::server_error(&e.to_string()))?;

        // Build address → chats mapping
        let chat_config = ChatSerializerConfig {
            include_participants: with_chat_participants,
            ..Default::default()
        };

        // For each handle, find matching chats and inject
        if let Some(arr) = results.as_array_mut() {
            for handle_json in arr.iter_mut() {
                let address = handle_json
                    .get("address")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let matching_chats: Vec<Value> = chats
                    .iter()
                    .filter(|chat| chat.participants.iter().any(|p| p.id == address))
                    .map(|chat| serialize_chat(chat, &chat_config, false))
                    .collect();

                if let Some(obj) = handle_json.as_object_mut() {
                    obj.insert("chats".to_string(), json!(matching_chats));
                }
            }
        }
    }

    let metadata = json!({
        "total": total,
        "offset": offset,
        "limit": limit,
        "count": handles.len(),
    });

    Ok(Json(success_response_with_metadata(results, metadata)))
}

/// GET /api/v1/handle/:guid
pub async fn find(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<Value>, AppError> {
    let (handles, _) = state
        .imessage_repo
        .lock()
        .get_handles(&HandleQueryParams {
            address: Some(address.clone()),
            limit: 1,
            offset: 0,
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let handle = handles
        .first()
        .ok_or_else(|| AppError::not_found("Handle not found!"))?;

    let data = serialize_handle(handle, false);
    Ok(Json(success_response(data)))
}

/// GET /api/v1/handle/:guid/focus [Private API required]
pub async fn get_focus_status(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<Value>, AppError> {
    let api = state.require_private_api()?;

    // Normalize address (matching check_availability behavior)
    let region = imessage_apple::process::get_region()
        .await
        .unwrap_or_else(|_| "US".to_string());
    let address = imessage_core::phone::normalize_address(&address, &region);

    // Verify the handle exists in the DB
    let (handles, _) = state
        .imessage_repo
        .lock()
        .get_handles(&HandleQueryParams {
            address: Some(address.clone()),
            limit: 1,
            offset: 0,
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    if handles.is_empty() {
        return Err(AppError::not_found("Handle not found!"));
    }

    let action = actions::get_focus_status(&address);
    let result = match api.send_action(action).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Focus status action failed: {e}");
            None
        }
    };

    // Transform response: { status: "unknown" | "silenced" | "none" }
    // The dylib sends {"silenced": true/false} (boolean); we check both bool and int forms.
    let status = match result {
        Some(txn) => match txn.data {
            Some(data) if !data.is_null() => {
                let silenced = data
                    .get("silenced")
                    .map(|v| v.as_bool().unwrap_or(false) || v.as_i64().unwrap_or(0) == 1)
                    .unwrap_or(false);
                if silenced { "silenced" } else { "none" }
            }
            _ => "unknown",
        },
        None => "unknown",
    };

    Ok(Json(success_response(json!({ "status": status }))))
}

/// GET /api/v1/handle/availability/imessage query params
#[derive(Debug, Deserialize)]
pub struct AvailabilityParams {
    pub address: String,
}

/// Shared availability check logic
async fn check_availability(
    state: &AppState,
    address: &str,
    action_fn: fn(&str) -> imessage_private_api::actions::Action,
) -> Result<Json<Value>, AppError> {
    let api = state.require_private_api()?;
    let region = imessage_apple::process::get_region()
        .await
        .unwrap_or_else(|_| "US".to_string());
    let normalized = imessage_core::phone::normalize_address(address, &region);
    let action = action_fn(&normalized);
    let result = api
        .send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    let available = match result {
        Some(txn) => match txn.data {
            Some(data) if !data.is_null() => data
                .get("available")
                .map(|v| v.as_bool().unwrap_or(false) || v.as_i64().unwrap_or(0) != 0)
                .unwrap_or(false),
            _ => {
                return Err(AppError::server_error(
                    "Failed to determine availability! No data returned.",
                ));
            }
        },
        None => {
            return Err(AppError::server_error(
                "Failed to determine availability! No response received.",
            ));
        }
    };

    Ok(Json(success_response(json!({ "available": available }))))
}

/// GET /api/v1/handle/availability/imessage [Private API required]
pub async fn get_imessage_availability(
    State(state): State<AppState>,
    Query(params): Query<AvailabilityParams>,
) -> Result<Json<Value>, AppError> {
    check_availability(&state, &params.address, actions::get_imessage_availability).await
}

/// POST /api/v1/handle/availability/imessage [Private API required]
pub async fn post_imessage_availability(
    State(state): State<AppState>,
    AppJson(body): AppJson<AvailabilityParams>,
) -> Result<Json<Value>, AppError> {
    check_availability(&state, &body.address, actions::get_imessage_availability).await
}

/// GET /api/v1/handle/availability/facetime [Private API required]
/// Note: dispatched by the Messages dylib (AccountActions), not FaceTime.
pub async fn get_facetime_availability(
    State(state): State<AppState>,
    Query(params): Query<AvailabilityParams>,
) -> Result<Json<Value>, AppError> {
    check_availability(&state, &params.address, actions::get_facetime_availability).await
}

/// POST /api/v1/handle/availability/facetime [Private API required]
/// Note: dispatched by the Messages dylib (AccountActions), not FaceTime.
pub async fn post_facetime_availability(
    State(state): State<AppState>,
    AppJson(body): AppJson<AvailabilityParams>,
) -> Result<Json<Value>, AppError> {
    check_availability(&state, &body.address, actions::get_facetime_availability).await
}
