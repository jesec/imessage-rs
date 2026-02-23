/// ?pretty query parameter middleware.
/// When `?pretty` is present in the query string, re-serializes JSON responses
/// with pretty-printing (indented).
use axum::body::Body;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use http_body_util::BodyExt;

pub async fn pretty_json_middleware(request: Request, next: Next) -> Response {
    let is_pretty = request.uri().query().is_some_and(|q| q.contains("pretty"));

    let response = next.run(request).await;

    if !is_pretty {
        return response;
    }

    // Only transform JSON responses
    let is_json = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("json"));

    if !is_json {
        return response;
    }

    // Extract the body
    let (parts, body) = response.into_parts();
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => return Response::from_parts(parts, Body::empty()),
    };

    // Try to parse and re-serialize with pretty-printing
    let pretty_bytes = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(value) => match serde_json::to_vec_pretty(&value) {
            Ok(pretty) => pretty,
            Err(_) => bytes.to_vec(),
        },
        Err(_) => bytes.to_vec(),
    };

    let mut response = Response::from_parts(parts, Body::from(pretty_bytes.clone()));
    response.headers_mut().insert(
        axum::http::header::CONTENT_LENGTH,
        pretty_bytes.len().into(),
    );
    response
}
