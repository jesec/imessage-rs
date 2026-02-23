use axum::Json;
/// iCloud routes:
///   GET  /api/v1/icloud/account                    [Private API]
///   POST /api/v1/icloud/account/alias              [Private API]
///   GET  /api/v1/icloud/contact                    [Private API]
///   GET  /api/v1/icloud/findmy/devices             [FindMy Private API]
///   POST /api/v1/icloud/findmy/devices/refresh     [FindMy Private API]
///   GET  /api/v1/icloud/findmy/friends             [Private API]
///   POST /api/v1/icloud/findmy/friends/refresh     [Private API]
use axum::extract::{Query, State};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::Deserialize;
use serde_json::{Value, json};

use imessage_private_api::actions;

use crate::extractors::AppJson;
use crate::middleware::error::{AppError, success_response, success_response_with_message};
use crate::state::AppState;

/// GET /api/v1/icloud/account [Private API]
pub async fn get_account_info(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let api = state.require_private_api()?;
    let action = actions::get_account_info();
    let result = api
        .send_action(action)
        .await
        .map_err(|e| AppError::server_error(&format!("Failed to fetch account info: {e}")))?;

    let data = result.and_then(|txn| txn.data).unwrap_or(json!(null));

    Ok(Json(success_response_with_message(
        "Successfully fetched account info!",
        data,
    )))
}

/// POST /api/v1/icloud/account/alias body
#[derive(Debug, Deserialize)]
pub struct ChangeAliasBody {
    pub alias: String,
}

/// POST /api/v1/icloud/account/alias [Private API]
pub async fn change_alias(
    State(state): State<AppState>,
    AppJson(body): AppJson<ChangeAliasBody>,
) -> Result<Json<Value>, AppError> {
    if body.alias.is_empty() {
        return Err(AppError::bad_request("An alias is required!"));
    }

    let api = state.require_private_api()?;

    // Get account info to validate the alias is in the vetted list
    let info_action = actions::get_account_info();
    let info_result = api
        .send_action(info_action)
        .await
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let info_data = info_result.and_then(|t| t.data).unwrap_or(json!(null));
    let vetted = info_data
        .get("vetted_aliases")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let aliases: Vec<String> = vetted
        .iter()
        .filter_map(|a| {
            a.get("Alias")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    if !aliases.contains(&body.alias) {
        return Err(AppError::bad_request(&format!(
            "Alias, \"{}\" is not assigned/enabled for your iCloud account!",
            body.alias
        )));
    }

    let action = actions::modify_active_alias(&body.alias);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    Ok(Json(success_response_with_message(
        "Successfully changed iMessage Alias!",
        json!(null),
    )))
}

/// GET /api/v1/icloud/contact query params
#[derive(Debug, Deserialize, Default)]
pub struct ContactCardParams {
    pub address: Option<String>,
}

/// GET /api/v1/icloud/contact [Private API]
pub async fn get_contact_card(
    State(state): State<AppState>,
    Query(params): Query<ContactCardParams>,
) -> Result<Json<Value>, AppError> {
    let address = params.address.as_deref().unwrap_or("");
    if address.is_empty() {
        return Err(AppError::bad_request("An address is required!"));
    }

    let api = state.require_private_api()?;
    let action = actions::get_contact_card(address);
    let result = api
        .send_action(action)
        .await
        .map_err(|e| AppError::server_error(&format!("Failed to fetch contact card: {e}")))?;

    let mut data = result.and_then(|t| t.data).unwrap_or(json!(null));

    // If avatar_path exists, read file → base64 encode, replace field
    if let Some(avatar_path) = data
        .get("avatar_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        && !avatar_path.is_empty()
        && let Ok(bytes) = std::fs::read(&avatar_path)
    {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        if let Some(obj) = data.as_object_mut() {
            obj.insert("avatar".to_string(), json!(b64));
            obj.remove("avatar_path");
        }
    }

    Ok(Json(success_response_with_message(
        "Successfully fetched contact card!",
        data,
    )))
}

/// GET /api/v1/icloud/findmy/devices [FindMy Private API]
/// Reads and decrypts the FindMy device cache from disk.
/// The decryption key is fetched on startup; if not yet available, this triggers a fetch.
pub async fn get_findmy_devices(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let key = get_or_fetch_findmy_key(&state).await?;
    let devices = decrypt_findmy_devices(&key)?;
    Ok(Json(success_response(devices)))
}

/// GET /api/v1/icloud/findmy/friends [Private API]
/// Returns cached friend locations. If cache is older than 5 minutes or empty,
/// triggers a refresh automatically.
pub async fn get_findmy_friends(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let needs_refresh = {
        let (ref map, ref last_refresh) = *state.findmy_friends_cache.lock();
        map.is_empty()
            || match last_refresh {
                None => true,
                Some(t) => t.elapsed() > std::time::Duration::from_secs(300),
            }
    };

    if needs_refresh {
        do_refresh_findmy_friends(&state).await?;
    }

    let cache = state.findmy_friends_cache.lock();
    let friends: Vec<&Value> = cache.0.values().collect();
    Ok(Json(success_response(json!(friends))))
}

/// POST /api/v1/icloud/findmy/devices/refresh [FindMy Private API]
/// Restarts FindMy.app to force a fresh FMIP server fetch, waits for the cache
/// to be updated, then re-reads and decrypts the device data.
pub async fn refresh_findmy_devices(
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    // Only allow one refresh at a time — each call kills FindMy.app and blocks for 12+ seconds
    let _guard = state
        .findmy_refresh_lock
        .try_lock()
        .map_err(|_| AppError::bad_request("A FindMy device refresh is already in progress"))?;

    // Ensure we already have the key before restarting FindMy
    let key = get_or_fetch_findmy_key(&state).await?;

    let api = state.require_findmy_private_api()?;

    // Kill FindMy.app and clear readiness before relaunching.
    // The 2s delay gives the background injection loop time to notice FindMy died,
    // cycle through its killall+sleep, and exit (since Messages is still connected).
    // Without this, the injection loop's killall can race with our relaunch.
    let _ = tokio::process::Command::new("killall")
        .arg("FindMy")
        .output()
        .await;
    api.clear_findmy_ready();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Relaunch FindMy.app with the helper dylib injected
    imessage_private_api::injection::relaunch_app_with_dylib("FindMy")
        .await
        .map_err(|e| AppError::server_error(&format!("Failed to relaunch FindMy.app: {e}")))?;

    // Wait for FindMy.app to reconnect and become ready (up to 30s)
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if api.is_findmy_ready() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::server_error(
                "FindMy.app did not become ready after restart (30s timeout)",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    // Wait for FindMy.app to fetch fresh device data from Apple servers
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    let devices = decrypt_findmy_devices(&key)?;
    Ok(Json(success_response_with_message(
        "Successfully refreshed FindMy devices!",
        devices,
    )))
}

/// POST /api/v1/icloud/findmy/friends/refresh [Private API]
pub async fn refresh_findmy_friends(
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    do_refresh_findmy_friends(&state).await?;

    let cache = state.findmy_friends_cache.lock();
    let data: Vec<Value> = cache.0.values().cloned().collect();
    Ok(Json(success_response_with_message(
        "Successfully refreshed FindMy friends!",
        json!(data),
    )))
}

/// Shared refresh logic: fetches friend locations from the dylib and updates the cache.
async fn do_refresh_findmy_friends(state: &AppState) -> Result<(), AppError> {
    let api = state.require_private_api()?;

    let action = actions::refresh_findmy_friends();
    let result = api
        .send_action(action)
        .await
        .map_err(|e| AppError::server_error(&format!("Failed to refresh FindMy friends: {e}")))?;

    if let Some(ref txn) = result
        && let Some(data) = &txn.data
        && let Some(locations) = data.get("locations").and_then(|v| v.as_array())
    {
        let mut cache = state.findmy_friends_cache.lock();
        cache.0.clear();
        for loc in locations {
            if let Some(handle) = loc.get("handle").and_then(|h| h.as_str()) {
                cache.0.insert(handle.to_string(), loc.clone());
            }
        }
        cache.1 = Some(std::time::Instant::now());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// FindMy key + device cache decryption
// ---------------------------------------------------------------------------

/// Get the cached FindMy decryption key, or fetch it from FindMy.app if not yet available.
async fn get_or_fetch_findmy_key(state: &AppState) -> Result<[u8; 32], AppError> {
    // Fast path: key already cached
    if let Some(key) = *state.findmy_key.lock() {
        return Ok(key);
    }
    // Slow path: fetch from FindMy.app's Keychain access
    fetch_findmy_key(state).await
}

/// Fetch the FindMy decryption key from the macOS Keychain via FindMy.app injection.
/// Caches the key in AppState for subsequent use.
async fn fetch_findmy_key(state: &AppState) -> Result<[u8; 32], AppError> {
    let api = state.require_findmy_private_api()?;

    let action = actions::get_findmy_key();
    let result = api
        .send_action(action)
        .await
        .map_err(|e| AppError::server_error(&format!("Failed to fetch FindMy key: {e}")))?;

    let b64_key = result
        .as_ref()
        .and_then(|txn| txn.data.as_ref())
        .and_then(|data| data.get("key"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::server_error("FindMy key not returned by helper"))?;

    use base64::Engine;
    let key_bytes = base64::engine::general_purpose::STANDARD
        .decode(b64_key)
        .map_err(|e| AppError::server_error(&format!("Invalid base64 key: {e}")))?;

    if key_bytes.len() != 32 {
        return Err(AppError::server_error(&format!(
            "FindMy key is {} bytes, expected 32",
            key_bytes.len()
        )));
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&key_bytes);
    *state.findmy_key.lock() = Some(key);
    Ok(key)
}

/// Read and decrypt the FindMy device + item caches from disk, merging into a single array.
///
/// Apple encrypts files in `~/Library/Caches/com.apple.findmy.fmipcore/` with
/// ChaCha20-Poly1305. Each file is a bplist with an `encryptedData` field containing
/// `nonce(12) || ciphertext || tag(16)`.
///
/// Reads Devices.data, Items.data, and ItemGroups.data. Items (AirTags, third-party
/// trackers) are transformed into the same shape as devices and merged into the array.
fn decrypt_findmy_devices(key: &[u8; 32]) -> Result<Value, AppError> {
    let home = home::home_dir()
        .ok_or_else(|| AppError::server_error("Cannot determine home directory"))?;
    let cache_dir = home.join("Library/Caches/com.apple.findmy.fmipcore");

    // Decrypt Devices.data (required)
    let devices_json = decrypt_findmy_file(key, &cache_dir, "Devices.data")?;
    let mut devices = match devices_json.as_array() {
        Some(arr) => arr.clone(),
        None => vec![devices_json],
    };

    // Decrypt Items.data (optional — AirTags, third-party trackers)
    if let Ok(items_json) = decrypt_findmy_file(key, &cache_dir, "Items.data")
        && let Some(items) = items_json.as_array()
    {
        // Build group identifier → name map from ItemGroups.data (optional)
        let group_map = decrypt_findmy_file(key, &cache_dir, "ItemGroups.data")
            .ok()
            .and_then(|v| v.as_array().cloned())
            .map(|groups| {
                groups
                    .iter()
                    .filter_map(|g| {
                        let id = g.get("identifier")?.as_str()?.to_string();
                        let name = g.get("name")?.as_str()?.to_string();
                        Some((id, name))
                    })
                    .collect::<std::collections::HashMap<String, String>>()
            })
            .unwrap_or_default();

        for item in items {
            devices.push(transform_findmy_item_to_device(item, &group_map));
        }
    }

    Ok(json!(devices))
}

/// Decrypt a single FindMy cache file (bplist with encryptedData → ChaCha20-Poly1305).
fn decrypt_findmy_file(
    key: &[u8; 32],
    cache_dir: &std::path::Path,
    filename: &str,
) -> Result<Value, AppError> {
    let path = cache_dir.join(filename);

    let data = std::fs::read(&path).map_err(|e| {
        AppError::server_error(&format!(
            "Failed to read {} at {}: {e}",
            filename,
            path.display()
        ))
    })?;

    let outer: plist::Value = plist::from_bytes(&data)
        .map_err(|e| AppError::server_error(&format!("Failed to parse {filename}: {e}")))?;

    let encrypted = outer
        .as_dictionary()
        .and_then(|d| d.get("encryptedData"))
        .and_then(|v| v.as_data())
        .ok_or_else(|| AppError::server_error(&format!("encryptedData not found in {filename}")))?;

    if encrypted.len() < 28 {
        return Err(AppError::server_error(&format!(
            "encryptedData too short in {filename}"
        )));
    }

    let (nonce_bytes, ciphertext_with_tag) = encrypted.split_at(12);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext_with_tag)
        .map_err(|_| {
            AppError::server_error(&format!(
                "Failed to decrypt {filename} (bad key or corrupted data)"
            ))
        })?;

    let inner: plist::Value = plist::from_bytes(&plaintext).map_err(|e| {
        AppError::server_error(&format!("Failed to parse decrypted {filename}: {e}"))
    })?;

    serde_json::to_value(inner)
        .map_err(|e| AppError::server_error(&format!("Failed to serialize {filename}: {e}")))
}

/// Transform a FindMyItem (AirTag, third-party tracker) into the FindMyDevice shape.
fn transform_findmy_item_to_device(
    item: &Value,
    group_map: &std::collections::HashMap<String, String>,
) -> Value {
    let product_type = item.get("productType");
    let type_str = product_type
        .and_then(|pt| pt.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");

    let model_display_name = if type_str == "b389" {
        "AirTag".to_string()
    } else {
        product_type
            .and_then(|pt| pt.get("productInformation"))
            .and_then(|pi| pi.get("modelName"))
            .and_then(|v| v.as_str())
            .unwrap_or(type_str)
            .to_string()
    };

    let lost_mode_metadata = item.get("lostModeMetadata").cloned().unwrap_or(json!(null));
    let lost_mode_enabled = !lost_mode_metadata.is_null();

    let group_identifier = item
        .get("groupIdentifier")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let group_name = group_identifier
        .as_deref()
        .and_then(|gid| group_map.get(gid))
        .cloned();

    json!({
        "deviceModel": type_str,
        "id": item.get("identifier"),
        "batteryStatus": "Unknown",
        "audioChannels": [],
        "lostModeCapable": true,
        "batteryLevel": item.get("batteryStatus"),
        "locationEnabled": true,
        "isConsideredAccessory": true,
        "address": item.get("address"),
        "location": item.get("location"),
        "modelDisplayName": model_display_name,
        "fmlyShare": false,
        "thisDevice": false,
        "lostModeEnabled": lost_mode_enabled,
        "deviceDisplayName": item.get("role").and_then(|r| r.get("emoji")),
        "safeLocations": item.get("safeLocations"),
        "name": item.get("name"),
        "isMac": false,
        "rawDeviceModel": type_str,
        "prsId": "owner",
        "locationCapable": true,
        "deviceClass": type_str,
        "crowdSourcedLocation": item.get("crowdSourcedLocation"),
        "identifier": item.get("identifier"),
        "productIdentifier": item.get("productIdentifier"),
        "role": item.get("role"),
        "serialNumber": item.get("serialNumber"),
        "lostModeMetadata": lost_mode_metadata,
        "groupIdentifier": group_identifier,
        "groupName": group_name,
        "isAppleAudioAccessory": item.get("isAppleAudioAccessory"),
        "capabilities": item.get("capabilities"),
    })
}
