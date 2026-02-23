use axum::Json;
/// FaceTime routes (requires FaceTime Private API):
///   POST /api/v1/facetime/session
///   POST /api/v1/facetime/answer/:call_uuid
///   POST /api/v1/facetime/leave/:call_uuid
use axum::extract::{Path, State};
use serde_json::{Value, json};

use imessage_private_api::actions;

use crate::facetime_session;
use crate::middleware::error::{AppError, success_response};
use crate::state::AppState;

/// POST /api/v1/facetime/session
///
/// Generate a new FaceTime link. The server briefly joins the call to act as
/// "host", admits remote users from the waiting room, then silently leaves.
pub async fn create_session(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let api = state.require_facetime_private_api()?;

    let link = facetime_session::create_session(&api)
        .await
        .map_err(|e| AppError::imessage_error(&e))?;

    Ok(Json(success_response(json!({ "link": link }))))
}

/// POST /api/v1/facetime/answer/:call_uuid
///
/// Answer an incoming FaceTime call, generate a shareable link, and run the
/// admit-and-leave flow in the background.
pub async fn answer_call(
    State(state): State<AppState>,
    Path(call_uuid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let api = state.require_facetime_private_api()?;

    let link = facetime_session::answer_call(&api, &call_uuid)
        .await
        .map_err(|e| AppError::imessage_error(&e))?;

    Ok(Json(success_response(json!({ "link": link }))))
}

/// POST /api/v1/facetime/leave/:call_uuid
/// Returns 201 with NoData.
pub async fn leave_call(
    State(state): State<AppState>,
    Path(call_uuid): Path<String>,
) -> Result<(axum::http::StatusCode, Json<Value>), AppError> {
    let api = state.require_facetime_private_api()?;
    let action = actions::leave_call(&call_uuid);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    // Return 201 "No Data"
    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({
            "status": 201,
            "message": "No Data",
        })),
    ))
}
