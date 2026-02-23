/// Authentication middleware.
///
/// Checks for `password`, `guid`, or `token` query param.
/// Compares against the configured server password (trimmed).
use axum::extract::{Query, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use serde::Deserialize;

use crate::middleware::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize, Default)]
pub struct AuthParams {
    pub password: Option<String>,
    pub guid: Option<String>,
    pub token: Option<String>,
}

pub async fn auth_middleware(
    State(state): State<AppState>,
    Query(params): Query<AuthParams>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    // Extract token from any of the three param names
    let token = params
        .guid
        .as_deref()
        .or(params.password.as_deref())
        .or(params.token.as_deref());

    let Some(token) = token else {
        return Err(AppError::unauthorized(Some("Missing server password!")));
    };

    // Ensure password is configured
    let password = &state.config.password;
    if password.is_empty() {
        return Err(AppError::server_error(
            "No password configured. Set one via --password or in config.yml",
        ));
    }

    // Compare (trimmed)
    if password.trim() != token.trim() {
        return Err(AppError::unauthorized(None));
    }

    Ok(next.run(request).await)
}
