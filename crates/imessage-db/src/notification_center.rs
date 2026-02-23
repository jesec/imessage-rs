/// Notification Center database reader for FaceTime join detection.
///
/// Reads from $(getconf DARWIN_USER_DIR)/com.apple.notificationcenter/db2/db
/// to detect when remote users enter a FaceTime waiting room (so we can
/// admit them via the Private API `admit-pending-member` action).
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, params};
use std::path::PathBuf;
use tracing::debug;

/// Cocoa epoch offset: seconds between Unix epoch (1970) and Apple epoch (2001).
const COCOA_EPOCH_OFFSET: f64 = 978307200.0;

/// A parsed FaceTime join notification containing the IDs needed
/// to call `admit-pending-member` via the Private API.
#[derive(Debug, Clone)]
pub struct FaceTimeJoinNotification {
    pub user_id: String,
    pub conversation_id: String,
}

/// Convert a Unix timestamp (seconds since 1970) to a Cocoa timestamp (seconds since 2001).
fn unix_to_cocoa(unix_secs: f64) -> f64 {
    unix_secs - COCOA_EPOCH_OFFSET
}

/// Get the current time as a Cocoa timestamp.
fn cocoa_now() -> f64 {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    unix_to_cocoa(unix)
}

/// Get the path to the Notification Center database.
fn get_db_path() -> Result<PathBuf> {
    let output = std::process::Command::new("/usr/bin/getconf")
        .arg("DARWIN_USER_DIR")
        .output()
        .context("Failed to run getconf DARWIN_USER_DIR")?;
    let base = String::from_utf8(output.stdout)
        .context("getconf output is not valid UTF-8")?
        .trim()
        .to_string();
    Ok(PathBuf::from(base).join("com.apple.notificationcenter/db2/db"))
}

/// Query the Notification Center DB for FaceTime join notifications
/// delivered since `lookback_secs` seconds ago.
///
/// Returns parsed join notifications with user IDs and conversation IDs.
pub fn get_facetime_join_notifications(
    lookback_secs: f64,
) -> Result<Vec<FaceTimeJoinNotification>> {
    let db_path = get_db_path()?;
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| {
        format!(
            "Failed to open NotificationCenter DB at {}",
            db_path.display()
        )
    })?;

    let since = cocoa_now() - lookback_secs;

    let mut stmt = conn.prepare(
        "SELECT record.data FROM record \
         LEFT JOIN app ON record.app_id = app.app_id \
         WHERE app.identifier = 'com.apple.facetime' \
         AND record.delivered_date >= ? \
         ORDER BY record.delivered_date ASC",
    )?;

    let rows = stmt.query_map(params![since], |row| {
        let data: Option<Vec<u8>> = row.get(0)?;
        Ok(data)
    })?;

    let mut notifications = Vec::new();
    for row in rows {
        if let Ok(Some(data)) = row {
            match parse_notification_data(&data) {
                Some(joins) => notifications.extend(joins),
                None => debug!(
                    "Could not parse notification center data blob ({} bytes)",
                    data.len()
                ),
            }
        }
    }

    Ok(notifications)
}

/// Parse notification data blob to extract FaceTime join info.
///
/// The data column can be in binary plist format (NSKeyedArchiver) or TypedStream.
/// We try binary plist first since that's the most likely format on macOS 15+.
fn parse_notification_data(data: &[u8]) -> Option<Vec<FaceTimeJoinNotification>> {
    // Try binary plist
    if let Ok(value) = plist::Value::from_reader(std::io::Cursor::new(data))
        && let Some(joins) = extract_joins_from_plist(&value)
    {
        return Some(joins);
    }

    // The data blob may contain multiple plist-encoded objects concatenated
    // or wrapped in a TypedStream. Try searching for embedded binary plists.
    if let Some(joins) = extract_joins_from_embedded_plists(data) {
        return Some(joins);
    }

    None
}

/// Extract FaceTime join information from a decoded plist value.
///
/// The notification data may be an NSKeyedArchiver archive with a `$objects` array.
/// Extracts userId from `$objects[6]` and conversationId from `$objects[9]`.
fn extract_joins_from_plist(value: &plist::Value) -> Option<Vec<FaceTimeJoinNotification>> {
    let dict = value.as_dictionary()?;

    // Check if this is an NSKeyedArchiver-style plist
    if let Some(objects) = dict.get("$objects").and_then(|v| v.as_array()) {
        return extract_joins_from_objects(objects);
    }

    // The data might be a plain dictionary with nested notification records.
    // Search recursively for any embedded plist data that contains join info.
    for (_, v) in dict.iter() {
        if let Some(data) = v.as_data()
            && let Ok(inner) = plist::Value::from_reader(std::io::Cursor::new(data))
            && let Some(joins) = extract_joins_from_plist(&inner)
        {
            return Some(joins);
        }
        // Recurse into nested dictionaries
        if v.as_dictionary().is_some()
            && let Some(joins) = extract_joins_from_plist(v)
        {
            return Some(joins);
        }
    }

    None
}

/// Extract join notifications from an NSKeyedArchiver `$objects` array.
///
/// Uses hardcoded indices: userId at [6], conversationId at [9].
/// We additionally verify that a "join" string exists in the objects.
fn extract_joins_from_objects(objects: &[plist::Value]) -> Option<Vec<FaceTimeJoinNotification>> {
    // Verify this is a join notification by looking for "join" in any string
    let has_join = objects.iter().any(|obj| {
        if let Some(s) = obj.as_string() {
            s.to_lowercase().contains("join")
        } else {
            false
        }
    });

    if !has_join || objects.len() < 10 {
        return None;
    }

    let user_id = objects.get(6)?.as_string()?;
    let conversation_id = objects.get(9)?.as_string()?;

    // Sanity check: both should look like UUIDs or identifiers
    if user_id.is_empty() || conversation_id.is_empty() {
        return None;
    }

    debug!(
        "Found FaceTime join: userId={}, conversationId={}",
        user_id, conversation_id
    );

    Some(vec![FaceTimeJoinNotification {
        user_id: user_id.to_string(),
        conversation_id: conversation_id.to_string(),
    }])
}

/// Search for embedded binary plists within a data blob.
///
/// The outer data format might be a TypedStream or other container.
/// We scan for `bplist00` magic bytes and try to parse each occurrence.
fn extract_joins_from_embedded_plists(data: &[u8]) -> Option<Vec<FaceTimeJoinNotification>> {
    let magic = b"bplist00";
    let mut offset = 0;

    while offset + magic.len() < data.len() {
        if let Some(pos) = data[offset..].windows(magic.len()).position(|w| w == magic) {
            let abs_pos = offset + pos;
            // Try to parse from this position to end of data
            if let Ok(value) = plist::Value::from_reader(std::io::Cursor::new(&data[abs_pos..]))
                && let Some(joins) = extract_joins_from_plist(&value)
            {
                return Some(joins);
            }
            offset = abs_pos + 1;
        } else {
            break;
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cocoa_epoch_is_correct() {
        // 2001-01-01 00:00:00 UTC as Unix timestamp
        assert_eq!(COCOA_EPOCH_OFFSET, 978307200.0);
    }

    #[test]
    fn unix_to_cocoa_conversion() {
        // 2025-01-01 00:00:00 UTC = 1735689600 Unix
        let cocoa = unix_to_cocoa(1735689600.0);
        // = 1735689600 - 978307200 = 757382400
        assert!((cocoa - 757382400.0).abs() < 0.001);
    }

    #[test]
    fn empty_data_returns_none() {
        assert!(parse_notification_data(&[]).is_none());
    }

    #[test]
    fn invalid_data_returns_none() {
        assert!(parse_notification_data(b"not a plist").is_none());
    }

    #[test]
    fn objects_without_join_returns_none() {
        // Construct a minimal plist with $objects but no "join" string
        let mut dict = plist::Dictionary::new();
        let objects: Vec<plist::Value> = (0..10)
            .map(|i| plist::Value::String(format!("item{i}")))
            .collect();
        dict.insert("$objects".to_string(), plist::Value::Array(objects));
        let value = plist::Value::Dictionary(dict);
        assert!(extract_joins_from_plist(&value).is_none());
    }
}
