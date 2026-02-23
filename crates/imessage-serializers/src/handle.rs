/// Handle serializer — converts a Handle entity to the API JSON shape.
///
/// Full response (non-notification):
/// {
///     "originalROWID": 1,
///     "address": "+15551234567",
///     "service": "iMessage",
///     "uncanonicalizedId": null,
///     "country": "us"
/// }
use serde_json::{Map, Value, json};

use imessage_db::imessage::entities::Handle;

/// Serialize a single Handle to JSON.
pub fn serialize_handle(handle: &Handle, is_for_notification: bool) -> Value {
    let mut map = Map::new();

    // Core fields (always present)
    map.insert("originalROWID".to_string(), json!(handle.rowid));
    map.insert("address".to_string(), json!(handle.id));
    map.insert("service".to_string(), json!(handle.service));

    // Non-notification fields
    if !is_for_notification {
        map.insert(
            "uncanonicalizedId".to_string(),
            json!(handle.uncanonicalized_id),
        );
        map.insert("country".to_string(), json!(handle.country));
    }

    Value::Object(map)
}

/// Serialize a list of Handles to JSON.
pub fn serialize_handles(handles: &[Handle], is_for_notification: bool) -> Value {
    let list: Vec<Value> = handles
        .iter()
        .map(|h| serialize_handle(h, is_for_notification))
        .collect();
    Value::Array(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_handle() -> Handle {
        Handle {
            rowid: 42,
            id: "+15551234567".to_string(),
            country: Some("us".to_string()),
            service: "iMessage".to_string(),
            uncanonicalized_id: Some("+15551234567".to_string()),
        }
    }

    #[test]
    fn serialize_full() {
        let json = serialize_handle(&test_handle(), false);
        assert_eq!(json["originalROWID"], 42);
        assert_eq!(json["address"], "+15551234567");
        assert_eq!(json["service"], "iMessage");
        assert_eq!(json["uncanonicalizedId"], "+15551234567");
        assert_eq!(json["country"], "us");
    }

    #[test]
    fn serialize_notification() {
        let json = serialize_handle(&test_handle(), true);
        assert_eq!(json["originalROWID"], 42);
        assert_eq!(json["address"], "+15551234567");
        assert!(json.get("country").is_none());
        assert!(json.get("uncanonicalizedId").is_none());
    }

    #[test]
    fn field_order_matches_nodejs() {
        let json = serialize_handle(&test_handle(), false);
        let serialized = serde_json::to_string(&json).unwrap();
        let rowid_pos = serialized.find("originalROWID").unwrap();
        let address_pos = serialized.find("address").unwrap();
        let service_pos = serialized.find("service").unwrap();
        assert!(rowid_pos < address_pos);
        assert!(address_pos < service_pos);
    }
}
