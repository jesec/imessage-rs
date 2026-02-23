use axum::extract::FromRequest;
/// Custom Axum extractors that convert rejections to our JSON error format.
use axum::extract::rejection::JsonRejection;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;

use crate::middleware::error::AppError;

/// A JSON extractor that returns our AppError format on deserialization failure.
pub struct AppJson<T>(pub T);

impl<T> IntoResponse for AppJson<T>
where
    T: serde::Serialize,
{
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

impl<S, T> FromRequest<S> for AppJson<T>
where
    axum::Json<T>: FromRequest<S, Rejection = JsonRejection>,
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(AppJson(value)),
            Err(rejection) => Err(AppError::bad_request(&rejection.body_text())),
        }
    }
}
