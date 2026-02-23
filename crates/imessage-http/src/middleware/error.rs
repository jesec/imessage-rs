/// Error types and error-to-JSON response conversion.
///
/// Response envelope format:
/// - Success: { status: 200, message: "Success", data: ..., metadata: ... }
/// - Error:   { status: N, message: "...", error: { type: "...", message: "..." } }
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

/// Error types (including the legacy DATABSE typo for API compatibility).
pub struct ErrorTypes;

impl ErrorTypes {
    pub const SERVER_ERROR: &'static str = "Server Error";
    pub const DATABASE_ERROR: &'static str = "Database Error"; // Legacy API had DATABSE_ERROR typo
    pub const IMESSAGE_ERROR: &'static str = "iMessage Error";
    pub const VALIDATION_ERROR: &'static str = "Validation Error";
    pub const AUTHENTICATION_ERROR: &'static str = "Authentication Error";
}

/// Application error that converts to a JSON response envelope.
#[derive(Debug)]
pub struct AppError {
    pub status: u16,
    pub message: String,
    pub error_type: &'static str,
    pub error_message: String,
    pub data: Option<Box<Value>>,
}

impl AppError {
    pub fn unauthorized(error: Option<&str>) -> Self {
        Self {
            status: 401,
            message: "You are not authorized to access this resource".to_string(),
            error_type: ErrorTypes::AUTHENTICATION_ERROR,
            error_message: error.unwrap_or("Unauthorized").to_string(),
            data: None,
        }
    }

    pub fn bad_request(error: &str) -> Self {
        Self {
            status: 400,
            message: "You've made a bad request! Please check your request params & body"
                .to_string(),
            error_type: ErrorTypes::VALIDATION_ERROR,
            error_message: error.to_string(),
            data: None,
        }
    }

    pub fn not_found(error: &str) -> Self {
        Self {
            status: 404,
            message: "The requested resource was not found".to_string(),
            error_type: ErrorTypes::DATABASE_ERROR,
            error_message: error.to_string(),
            data: None,
        }
    }

    pub fn server_error(error: &str) -> Self {
        Self {
            status: 500,
            message: "The server has encountered an error".to_string(),
            error_type: ErrorTypes::SERVER_ERROR,
            error_message: error.to_string(),
            data: None,
        }
    }

    pub fn imessage_error(error: &str) -> Self {
        Self {
            status: 500,
            message: "iMessage has encountered an error".to_string(),
            error_type: ErrorTypes::IMESSAGE_ERROR,
            error_message: error.to_string(),
            data: None,
        }
    }

    /// iMessage error with attached data (e.g., the sent message that had an error).
    pub fn imessage_error_with_data(error: &str, data: Value) -> Self {
        Self {
            status: 500,
            message: error.to_string(),
            error_type: ErrorTypes::IMESSAGE_ERROR,
            error_message: "Message failed to send!".to_string(),
            data: Some(Box::new(data)),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        let mut map = Map::new();
        map.insert("status".to_string(), json!(self.status));
        map.insert("message".to_string(), json!(self.message));
        map.insert(
            "error".to_string(),
            json!({
                "type": self.error_type,
                "message": self.error_message,
            }),
        );
        if let Some(data) = self.data {
            map.insert("data".to_string(), *data);
        }

        (status, axum::Json(Value::Object(map))).into_response()
    }
}

/// Build a success response envelope.
pub fn success_response(data: Value) -> Value {
    let mut map = Map::new();
    map.insert("status".to_string(), json!(200));
    map.insert("message".to_string(), json!("Success"));
    map.insert("data".to_string(), data);
    Value::Object(map)
}

/// Build a success response with custom message.
pub fn success_response_with_message(message: &str, data: Value) -> Value {
    let mut map = Map::new();
    map.insert("status".to_string(), json!(200));
    map.insert("message".to_string(), json!(message));
    map.insert("data".to_string(), data);
    Value::Object(map)
}

/// Build a success response with metadata.
pub fn success_response_with_metadata(data: Value, metadata: Value) -> Value {
    let mut map = Map::new();
    map.insert("status".to_string(), json!(200));
    map.insert("message".to_string(), json!("Success"));
    map.insert("data".to_string(), data);
    map.insert("metadata".to_string(), metadata);
    Value::Object(map)
}

/// Build a success response with custom message and metadata.
pub fn success_with_message_and_metadata(message: &str, data: Value, metadata: Value) -> Value {
    let mut map = Map::new();
    map.insert("status".to_string(), json!(200));
    map.insert("message".to_string(), json!(message));
    map.insert("data".to_string(), data);
    map.insert("metadata".to_string(), metadata);
    Value::Object(map)
}
