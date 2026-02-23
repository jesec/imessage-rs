/// Incoming event types from the helper dylib.
use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

/// A raw event received from the helper dylib over TCP.
#[derive(Debug, Clone, Deserialize)]
pub struct RawEvent {
    /// Event type (e.g., "ping", "started-typing", "facetime-call-status-changed")
    pub event: Option<String>,

    /// Transaction ID (present if this is a response to an outgoing action)
    #[serde(rename = "transactionId")]
    pub transaction_id: Option<String>,

    /// Message/entity identifier returned in transaction responses
    pub identifier: Option<String>,

    /// Chat GUID (for typing events, etc.)
    pub guid: Option<String>,

    /// Event data payload
    pub data: Option<Value>,

    /// Error message (for failed transactions)
    pub error: Option<String>,

    /// Process bundle identifier (for ping events)
    pub process: Option<String>,

    /// Extra fields not covered above (e.g. "url", "silenced", "available").
    /// The dylib often puts response data as top-level keys rather than
    /// inside a "data" wrapper. This captures those fields so we can
    /// merge them into the transaction result.
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl RawEvent {
    /// Check if this is a transaction response (has transactionId).
    pub fn is_transaction_response(&self) -> bool {
        self.transaction_id.is_some()
    }

    /// Check if this is an event (has event field).
    pub fn is_event(&self) -> bool {
        self.event.is_some()
    }

    /// Extract the response data.
    ///
    /// The dylib puts data in one of two shapes:
    ///   1. `{"transactionId": "…", "data": { … }}`  — explicit `data` wrapper
    ///   2. `{"transactionId": "…", "url": "…"}`      — top-level fields
    ///
    /// If an explicit `data` field exists, use it. Otherwise collect all extra
    /// fields (those not consumed by named struct fields) into a JSON object.
    pub fn extract_data(&self) -> Option<Value> {
        if self.data.is_some() {
            return self.data.clone();
        }
        if !self.extra.is_empty() {
            let map: serde_json::Map<String, Value> = self
                .extra
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Some(Value::Object(map))
        } else {
            None
        }
    }
}

/// All known incoming event types.
pub mod event_types {
    pub const PING: &str = "ping";
    pub const READY: &str = "ready";
    pub const STARTED_TYPING: &str = "started-typing";
    pub const TYPING: &str = "typing";
    pub const STOPPED_TYPING: &str = "stopped-typing";
    pub const ALIASES_REMOVED: &str = "aliases-removed";
    pub const FACETIME_CALL_STATUS_CHANGED: &str = "facetime-call-status-changed";
    pub const NEW_FINDMY_LOCATION: &str = "new-findmy-location";
}

/// Parsed typing event.
#[derive(Debug, Clone)]
pub struct TypingEvent {
    pub guid: String,
    pub is_typing: bool,
}

/// Parsed FaceTime call status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceTimeStatus {
    Unknown = 0,
    Answered = 1,
    Outgoing = 3,
    Incoming = 4,
    Disconnected = 6,
}

impl FaceTimeStatus {
    pub fn from_i64(val: i64) -> Self {
        match val {
            1 => Self::Answered,
            3 => Self::Outgoing,
            4 => Self::Incoming,
            6 => Self::Disconnected,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Answered => "answered",
            Self::Outgoing => "outgoing",
            Self::Incoming => "incoming",
            Self::Disconnected => "disconnected",
        }
    }
}

/// Parsed FaceTime call event.
#[derive(Debug, Clone)]
pub struct FaceTimeEvent {
    pub call_uuid: String,
    pub status: FaceTimeStatus,
    pub status_id: i64,
    pub address: String,
    pub ended_error: Option<String>,
    pub ended_reason: Option<String>,
    pub image_url: Option<String>,
    pub is_outgoing: bool,
    pub is_audio: bool,
    pub is_video: bool,
}

/// Parsed FindMy location item.
#[derive(Debug, Clone)]
pub struct FindMyLocation {
    pub handle: String,
    pub coordinates: (f64, f64),
    pub long_address: Option<String>,
    pub short_address: Option<String>,
    pub subtitle: Option<String>,
    pub title: Option<String>,
    pub last_updated: Option<i64>,
    pub is_locating_in_progress: bool,
    pub status: String,
}

/// Parse a typing event from a raw event.
pub fn parse_typing_event(raw: &RawEvent) -> Option<TypingEvent> {
    let event_type = raw.event.as_deref()?;
    let guid = raw.guid.as_deref()?;

    // Skip group chats (GUIDs containing ";+;")
    if guid.contains(";+;") {
        return None;
    }

    let is_typing = matches!(event_type, "started-typing" | "typing");
    Some(TypingEvent {
        guid: guid.to_string(),
        is_typing,
    })
}

/// Parse a FaceTime event from raw event data.
pub fn parse_facetime_event(raw: &RawEvent) -> Option<FaceTimeEvent> {
    let data = raw.data.as_ref()?;

    let call_status = data.get("call_status")?.as_i64()?;
    let call_uuid = data
        .get("call_uuid")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let address = data
        .get("handle")
        .and_then(|h| h.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let ended_error = data
        .get("ended_error")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let ended_reason = data
        .get("ended_reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let image_url = data
        .get("image_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let is_outgoing = data
        .get("is_outgoing")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let is_audio = data
        .get("is_sending_audio")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let is_video = data
        .get("is_sending_video")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Some(FaceTimeEvent {
        call_uuid,
        status: FaceTimeStatus::from_i64(call_status),
        status_id: call_status,
        address,
        ended_error,
        ended_reason,
        image_url,
        is_outgoing,
        is_audio,
        is_video,
    })
}

/// Parse FindMy locations from raw event data.
pub fn parse_findmy_locations(raw: &RawEvent) -> Vec<FindMyLocation> {
    let Some(data) = raw.data.as_ref() else {
        return vec![];
    };
    let Some(items) = data.as_array() else {
        return vec![];
    };

    items
        .iter()
        .filter_map(|item| {
            let handle = item.get("handle")?.as_str()?.to_string();
            let coords = item.get("coordinates")?.as_array()?;
            let lat = coords.first()?.as_f64()?;
            let lon = coords.get(1)?.as_f64()?;

            Some(FindMyLocation {
                handle,
                coordinates: (lat, lon),
                long_address: item
                    .get("long_address")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                short_address: item
                    .get("short_address")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                subtitle: item
                    .get("subtitle")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                title: item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                last_updated: item.get("last_updated").and_then(|v| v.as_i64()),
                is_locating_in_progress: item
                    .get("is_locating_in_progress")
                    .and_then(|v| v.as_bool().or_else(|| v.as_i64().map(|n| n != 0)))
                    .unwrap_or(false),
                status: item
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw_event(overrides: impl FnOnce(&mut RawEvent)) -> RawEvent {
        let mut e = RawEvent {
            event: None,
            transaction_id: None,
            identifier: None,
            guid: None,
            data: None,
            error: None,
            process: None,
            extra: HashMap::new(),
        };
        overrides(&mut e);
        e
    }

    #[test]
    fn parse_typing_dm() {
        let raw = raw_event(|e| {
            e.event = Some("started-typing".into());
            e.guid = Some("iMessage;-;+15551234567".into());
        });
        let result = parse_typing_event(&raw).unwrap();
        assert!(result.is_typing);
        assert_eq!(result.guid, "iMessage;-;+15551234567");
    }

    #[test]
    fn parse_typing_skips_group() {
        let raw = raw_event(|e| {
            e.event = Some("started-typing".into());
            e.guid = Some("iMessage;+;chat123456".into());
        });
        assert!(parse_typing_event(&raw).is_none());
    }

    #[test]
    fn parse_stopped_typing() {
        let raw = raw_event(|e| {
            e.event = Some("stopped-typing".into());
            e.guid = Some("iMessage;-;+15551234567".into());
        });
        let result = parse_typing_event(&raw).unwrap();
        assert!(!result.is_typing);
    }

    #[test]
    fn parse_facetime_incoming() {
        let raw = raw_event(|e| {
            e.event = Some("facetime-call-status-changed".into());
            e.data = Some(json!({
                "call_status": 4,
                "call_uuid": "abc-123",
                "handle": { "value": "+15551234567" },
                "is_outgoing": false,
                "is_sending_audio": true,
                "is_sending_video": false
            }));
        });
        let facetime_event = parse_facetime_event(&raw).unwrap();
        assert_eq!(facetime_event.status, FaceTimeStatus::Incoming);
        assert_eq!(facetime_event.call_uuid, "abc-123");
        assert_eq!(facetime_event.address, "+15551234567");
        assert!(facetime_event.is_audio);
        assert!(!facetime_event.is_video);
    }

    #[test]
    fn parse_findmy_locations_multiple() {
        let raw = raw_event(|e| {
            e.event = Some("new-findmy-location".into());
            e.data = Some(json!([
                {
                    "handle": "+15551234567",
                    "coordinates": [37.7749, -122.4194],
                    "long_address": "San Francisco, CA",
                    "status": "live"
                },
                {
                    "handle": "user@icloud.com",
                    "coordinates": [40.7128, -74.0060],
                    "status": "legacy"
                }
            ]));
        });
        let locations = parse_findmy_locations(&raw);
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].handle, "+15551234567");
        assert_eq!(locations[0].status, "live");
        assert_eq!(locations[1].handle, "user@icloud.com");
    }

    #[test]
    fn facetime_status_from_i64() {
        assert_eq!(FaceTimeStatus::from_i64(0), FaceTimeStatus::Unknown);
        assert_eq!(FaceTimeStatus::from_i64(1), FaceTimeStatus::Answered);
        assert_eq!(FaceTimeStatus::from_i64(4), FaceTimeStatus::Incoming);
        assert_eq!(FaceTimeStatus::from_i64(6), FaceTimeStatus::Disconnected);
        assert_eq!(FaceTimeStatus::from_i64(99), FaceTimeStatus::Unknown);
    }

    #[test]
    fn extract_data_prefers_explicit_data_field() {
        let raw = raw_event(|e| {
            e.data = Some(json!({"links": []}));
            e.extra.insert("stray".into(), json!("ignored"));
        });
        assert_eq!(raw.extract_data(), Some(json!({"links": []})));
    }

    #[test]
    fn extract_data_falls_back_to_extra_fields() {
        // Matches dylib responses like {"transactionId": "…", "url": "https://…"}
        let raw = raw_event(|e| {
            e.extra.insert(
                "url".into(),
                json!("https://facetime.apple.com/join#v=1&abc"),
            );
        });
        let data = raw.extract_data().unwrap();
        assert_eq!(data["url"], "https://facetime.apple.com/join#v=1&abc");
    }

    #[test]
    fn extract_data_returns_none_when_empty() {
        let raw = raw_event(|_| {});
        assert!(raw.extract_data().is_none());
    }

    #[test]
    fn extract_data_from_deserialized_json() {
        // Simulate what serde does with a real dylib response
        let json_str = r#"{"transactionId":"abc","silenced":true}"#;
        let raw: RawEvent = serde_json::from_str(json_str).unwrap();
        assert!(raw.transaction_id.as_deref() == Some("abc"));
        assert!(raw.data.is_none()); // no "data" key
        let data = raw.extract_data().unwrap();
        assert_eq!(data["silenced"], true);
    }
}
