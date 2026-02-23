use axum::Json;
/// Server routes:
///   GET /api/v1/server/info
///   GET /api/v1/server/logs
///   GET /api/v1/server/permissions
///   GET /api/v1/server/statistics/totals
///   GET /api/v1/server/statistics/media
use axum::extract::{Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use imessage_core::config::AppPaths;
use imessage_core::macos::macos_version;

use crate::middleware::error::{AppError, success_response};
use crate::state::AppState;

/// GET /api/v1/server/info
pub async fn get_info(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let os_version = macos_version();

    let detected_icloud = imessage_apple::process::get_icloud_account()
        .await
        .unwrap_or_default();

    let detected_imessage = state
        .imessage_repo
        .lock()
        .get_imessage_account()
        .ok()
        .flatten()
        .unwrap_or_default();

    // Computer ID: user@hostname
    let hostname = std::process::Command::new("hostname")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let username = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
    let computer_id = format!("{username}@{hostname}");

    // Local IPs
    let (ipv4s, ipv6s) = get_local_ips();

    let helper_connected = if let Some(ref api) = state.private_api {
        api.is_connected().await
    } else {
        false
    };

    let private_api_ready = state
        .private_api
        .as_ref()
        .is_some_and(|api| api.is_messages_ready());
    let facetime_private_api_ready = state
        .private_api
        .as_ref()
        .is_some_and(|api| api.is_facetime_ready());
    let findmy_private_api_ready = state
        .private_api
        .as_ref()
        .is_some_and(|api| api.is_findmy_ready());

    // macOS time sync: query NTP offset via sntp
    let time_sync = get_time_sync_offset().await;

    let data = json!({
        "computer_id": computer_id,
        "os_version": os_version.to_string(),
        "server_version": AppPaths::version(),
        "private_api": state.config.enable_private_api,
        "helper_connected": helper_connected,
        "private_api_ready": private_api_ready,
        "facetime_private_api_ready": facetime_private_api_ready,
        "findmy_private_api_ready": findmy_private_api_ready,
        "detected_icloud": detected_icloud,
        "detected_imessage": detected_imessage,
        "macos_time_sync": time_sync,
        "local_ipv4s": ipv4s,
        "local_ipv6s": ipv6s,
    });

    Ok(Json(success_response(data)))
}

/// GET /api/v1/server/statistics/totals query params
#[derive(Debug, Deserialize, Default)]
pub struct StatTotalsParams {
    pub only: Option<String>,
}

/// GET /api/v1/server/statistics/totals
pub async fn get_stat_totals(
    State(state): State<AppState>,
    Query(params): Query<StatTotalsParams>,
) -> Result<Json<Value>, AppError> {
    let repo = state.imessage_repo.lock();

    // Parse ?only= filter (comma-separated list of stat names).
    // Strip trailing 's' from filter values ("messages" -> "message") for API compat.
    // We normalize both the filter values and keys to singular form.
    let only_filter: Option<Vec<String>> = params.only.as_ref().map(|s| {
        s.split(',')
            .map(|item| {
                let trimmed = item.trim().to_lowercase();
                trimmed.strip_suffix('s').unwrap_or(&trimmed).to_string()
            })
            .collect()
    });

    let should_include = |key: &str| -> bool {
        only_filter.as_ref().is_none_or(|f| {
            let normalized = key.to_lowercase();
            let singular = normalized.strip_suffix('s').unwrap_or(&normalized);
            f.iter().any(|v| v == singular)
        })
    };

    let mut data = serde_json::Map::new();
    if should_include("handles") {
        data.insert(
            "handles".to_string(),
            json!(repo.get_handle_count(None).unwrap_or(0)),
        );
    }
    if should_include("messages") {
        data.insert(
            "messages".to_string(),
            json!(repo.get_message_count(&Default::default()).unwrap_or(0)),
        );
    }
    if should_include("chats") {
        data.insert(
            "chats".to_string(),
            json!(repo.get_chat_count().unwrap_or(0)),
        );
    }
    if should_include("attachments") {
        data.insert(
            "attachments".to_string(),
            json!(repo.get_attachment_count().unwrap_or(0)),
        );
    }

    Ok(Json(success_response(Value::Object(data))))
}

/// GET /api/v1/server/statistics/media query params
#[derive(Debug, Deserialize, Default)]
pub struct StatMediaParams {
    pub only: Option<String>,
}

/// GET /api/v1/server/statistics/media
pub async fn get_stat_media(
    State(state): State<AppState>,
    Query(params): Query<StatMediaParams>,
) -> Result<Json<Value>, AppError> {
    // Parse ?only= filter (comma-separated). Strip trailing 's' and lowercase.
    let only_filter: Option<Vec<String>> = params.only.as_ref().map(|s| {
        s.split(',')
            .map(|item| {
                let trimmed = item.trim().to_lowercase();
                trimmed.strip_suffix('s').unwrap_or(&trimmed).to_string()
            })
            .collect()
    });

    let should_include = |key: &str| -> bool {
        only_filter
            .as_ref()
            .is_none_or(|f| f.iter().any(|v| v == key))
    };

    let repo = state.imessage_repo.lock();
    let mut data = serde_json::Map::new();

    if should_include("image") {
        data.insert(
            "images".to_string(),
            json!(repo.get_media_counts("image", None).unwrap_or(0)),
        );
    }
    if should_include("video") {
        data.insert(
            "videos".to_string(),
            json!(repo.get_media_counts("video", None).unwrap_or(0)),
        );
    }
    if should_include("location") {
        data.insert(
            "locations".to_string(),
            json!(repo.get_media_counts("location", None).unwrap_or(0)),
        );
    }

    Ok(Json(success_response(Value::Object(data))))
}

/// GET /api/v1/server/statistics/media/chat query params
#[derive(Debug, Deserialize, Default)]
pub struct StatMediaByChatParams {
    #[serde(alias = "chatGuid")]
    pub chat_guid: Option<String>,
    pub only: Option<String>,
}

/// GET /api/v1/server/statistics/media/chat
pub async fn get_stat_media_by_chat(
    State(state): State<AppState>,
    Query(params): Query<StatMediaByChatParams>,
) -> Result<Json<Value>, AppError> {
    let chat_guid = params
        .chat_guid
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("A chatGuid is required!"))?;

    let only_filter: Option<Vec<String>> = params.only.as_ref().map(|s| {
        s.split(',')
            .map(|item| {
                let trimmed = item.trim().to_lowercase();
                trimmed.strip_suffix('s').unwrap_or(&trimmed).to_string()
            })
            .collect()
    });

    let should_include = |key: &str| -> bool {
        only_filter
            .as_ref()
            .is_none_or(|f| f.iter().any(|v| v == key))
    };

    let repo = state.imessage_repo.lock();
    let mut data = serde_json::Map::new();

    if should_include("image") {
        data.insert(
            "images".to_string(),
            json!(
                repo.get_media_counts_by_chat(chat_guid, "image")
                    .unwrap_or(0)
            ),
        );
    }
    if should_include("video") {
        data.insert(
            "videos".to_string(),
            json!(
                repo.get_media_counts_by_chat(chat_guid, "video")
                    .unwrap_or(0)
            ),
        );
    }
    if should_include("location") {
        data.insert(
            "locations".to_string(),
            json!(
                repo.get_media_counts_by_chat(chat_guid, "location")
                    .unwrap_or(0)
            ),
        );
    }

    Ok(Json(success_response(Value::Object(data))))
}

/// GET /api/v1/server/logs query params
#[derive(Debug, Deserialize, Default)]
pub struct LogsParams {
    pub count: Option<String>,
}

/// GET /api/v1/server/logs
pub async fn get_logs(Query(params): Query<LogsParams>) -> Result<Json<Value>, AppError> {
    let count = params
        .count
        .as_deref()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(100);

    // Read the last N lines from the log file
    let log_path = AppPaths::user_data().join("logs").join("main.log");
    let log_text = if log_path.exists() {
        imessage_apple::process::exec_shell_command(&format!(
            "tail -n {} \"{}\"",
            count,
            log_path.display()
        ))
        .await
        .unwrap_or_default()
    } else {
        String::new()
    };

    Ok(Json(success_response(json!(log_text))))
}

/// GET /api/v1/server/permissions
pub async fn get_permissions(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    // Full Disk Access: if DB opened successfully, we have it
    let has_disk_access = true;

    // SIP status
    let sip_disabled = imessage_apple::process::is_sip_disabled()
        .await
        .unwrap_or(false);

    // Private API readiness
    let private_api_ready = state
        .private_api
        .as_ref()
        .is_some_and(|api| api.is_messages_ready());

    // Check Accessibility permission via AXIsProcessTrusted
    let has_accessibility = check_accessibility();

    let data = json!([
        {
            "name": "Full Disk Access",
            "pass": has_disk_access,
            "pane": "FullDiskAccess",
            "solution": "Ensure that the server has Full Disk Access in System Settings > Privacy & Security"
        },
        {
            "name": "Accessibility",
            "pass": has_accessibility,
            "pane": "Accessibility",
            "optional": true,
            "solution": "Ensure that the server has Accessibility access in System Settings > Privacy & Security"
        },
        {
            "name": "SIP Disabled",
            "pass": sip_disabled,
            "optional": !state.config.enable_private_api,
            "solution": "SIP must be disabled for the Private API to work. Restart into Recovery Mode and run: csrutil disable"
        },
        {
            "name": "Private API",
            "pass": private_api_ready,
            "optional": !state.config.enable_private_api,
            "solution": "The Private API helper must be connected for Private API features to work"
        }
    ]);

    Ok(Json(success_response(data)))
}

/// Get macOS NTP time sync offset in seconds.
/// Runs `sntp -t 5 time.apple.com` and parses the offset.
async fn get_time_sync_offset() -> Value {
    let output =
        imessage_apple::process::exec_shell_command("/usr/bin/sntp -t 5 time.apple.com 2>&1").await;

    match output {
        Ok(text) => {
            // Output format: "+0.001234 +/- 0.005678" or similar
            // We want the first number (the offset in seconds)
            for line in text.lines() {
                let trimmed = line.trim();
                // Look for a line with a numeric offset
                if let Some(first_token) = trimmed.split_whitespace().next()
                    && let Ok(offset_secs) = first_token.parse::<f64>()
                {
                    return json!(offset_secs);
                }
            }
            Value::Null
        }
        Err(_) => Value::Null,
    }
}

/// Check if the process has Accessibility permissions via AXIsProcessTrusted.
fn check_accessibility() -> bool {
    // Use the ApplicationServices framework's AXIsProcessTrusted
    let output = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to return (exists process 1)",
        ])
        .output();

    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Enumerate all non-loopback network interfaces.
fn get_local_ips() -> (Vec<String>, Vec<String>) {
    let mut ipv4s = Vec::new();
    let mut ipv6s = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if iface.is_loopback() {
                continue;
            }
            match iface.addr {
                if_addrs::IfAddr::V4(ref v4) => ipv4s.push(v4.ip.to_string()),
                if_addrs::IfAddr::V6(ref v6) => ipv6s.push(v6.ip.to_string()),
            }
        }
    }
    (ipv4s, ipv6s)
}
