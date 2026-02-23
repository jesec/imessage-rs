/// Chat serializer — converts a Chat entity to the API JSON shape.
///
/// Full response (non-notification):
/// {
///     "originalROWID": 1,
///     "guid": "iMessage;-;+15551234567",
///     "style": 45,
///     "chatIdentifier": "+15551234567",
///     "isArchived": false,
///     "displayName": null,
///     "participants": [...],
///     "isFiltered": false,
///     "groupId": "...",
///     "properties": null,
///     "lastAddressedHandle": null
/// }
use serde_json::{Map, Value, json};

use imessage_db::imessage::entities::Chat;

use crate::config::ChatSerializerConfig;
use crate::handle::serialize_handles;
use crate::plist_decode;

/// Serialize a single Chat to JSON.
pub fn serialize_chat(
    chat: &Chat,
    config: &ChatSerializerConfig,
    is_for_notification: bool,
) -> Value {
    let mut map = Map::new();

    // Core fields (always present)
    map.insert("originalROWID".to_string(), json!(chat.rowid));
    map.insert("guid".to_string(), json!(chat.guid));
    map.insert("style".to_string(), json!(chat.style));
    map.insert("chatIdentifier".to_string(), json!(chat.chat_identifier));
    map.insert("isArchived".to_string(), json!(chat.is_archived));
    map.insert("displayName".to_string(), json!(chat.display_name));

    // Participants
    if config.include_participants {
        map.insert(
            "participants".to_string(),
            serialize_handles(&chat.participants, is_for_notification),
        );
    }

    // Messages (always empty array in list context, populated on demand)
    if config.include_messages {
        map.insert("messages".to_string(), json!([]));
    }

    // Non-notification fields
    if !is_for_notification {
        map.insert("isFiltered".to_string(), json!(chat.is_filtered));
        map.insert("groupId".to_string(), json!(chat.group_id));
        // Decode the properties blob (binary plist) to JSON
        let properties = chat
            .properties
            .as_deref()
            .and_then(plist_decode::decode_chat_properties)
            .unwrap_or(Value::Null);
        map.insert("properties".to_string(), properties);
        map.insert(
            "lastAddressedHandle".to_string(),
            json!(chat.last_addressed_handle),
        );
    }

    Value::Object(map)
}

/// Serialize a list of Chats to JSON.
pub fn serialize_chats(
    chats: &[Chat],
    config: &ChatSerializerConfig,
    is_for_notification: bool,
) -> Value {
    let list: Vec<Value> = chats
        .iter()
        .map(|c| serialize_chat(c, config, is_for_notification))
        .collect();
    Value::Array(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_chat() -> Chat {
        Chat {
            rowid: 1,
            guid: "iMessage;-;+15551234567".to_string(),
            style: 45,
            chat_identifier: Some("+15551234567".to_string()),
            is_archived: false,
            display_name: None,
            is_filtered: false,
            group_id: None,
            last_addressed_handle: None,
            ..Default::default()
        }
    }

    #[test]
    fn serialize_full_chat() {
        let config = ChatSerializerConfig::default();
        let json = serialize_chat(&test_chat(), &config, false);
        assert_eq!(json["originalROWID"], 1);
        assert_eq!(json["guid"], "iMessage;-;+15551234567");
        assert_eq!(json["style"], 45);
        assert_eq!(json["isArchived"], false);
        assert!(json.get("isFiltered").is_some());
        assert!(json.get("groupId").is_some());
        assert!(json.get("properties").is_some());
        assert!(json.get("lastAddressedHandle").is_some());
    }

    #[test]
    fn serialize_notification_chat() {
        let config = ChatSerializerConfig::default();
        let json = serialize_chat(&test_chat(), &config, true);
        assert_eq!(json["originalROWID"], 1);
        assert!(json.get("isFiltered").is_none());
        assert!(json.get("groupId").is_none());
    }

    #[test]
    fn participants_included_by_default() {
        let config = ChatSerializerConfig::default();
        let json = serialize_chat(&test_chat(), &config, false);
        assert!(json.get("participants").is_some());
        assert!(json["participants"].is_array());
    }

    #[test]
    fn participants_excluded_when_configured() {
        let config = ChatSerializerConfig {
            include_participants: false,
            ..Default::default()
        };
        let json = serialize_chat(&test_chat(), &config, false);
        assert!(json.get("participants").is_none());
    }
}
