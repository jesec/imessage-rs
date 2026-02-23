/// Message serializer — converts a Message entity to the API JSON shape.
///
/// This is the most complex serializer. The field order and conditional
/// inclusion logic must match the API contract exactly.
///
/// The response is built in this specific order:
/// 1. Core fields (always present)
/// 2. Non-notification fields (conditionally added)
/// 3. Monterey+ fields (wasDeliveredQuietly, didNotifyRecipient)
/// 4. Chats (if config.includeChats)
/// 5. High Sierra+ fields (messageSummaryInfo, payloadData)
/// 6. Ventura+ fields (dateEdited, dateRetracted, partCount)
use serde_json::{Map, Value, json};

use imessage_core::typedstream;
use imessage_db::imessage::entities::Message;

use crate::attachment::serialize_attachments;
use crate::chat::serialize_chats;
use crate::config::{AttachmentSerializerConfig, ChatSerializerConfig, MessageSerializerConfig};
use crate::handle::serialize_handle;
use crate::plist_decode;

/// Serialize a single Message to JSON.
pub fn serialize_message(
    message: &Message,
    config: &MessageSerializerConfig,
    attachment_config: &AttachmentSerializerConfig,
    is_for_notification: bool,
) -> Value {
    let mut map = Map::new();

    // --- Core fields (always present) ---
    map.insert("originalROWID".to_string(), json!(message.rowid));
    map.insert("guid".to_string(), json!(message.guid));

    // text: universalText logic — use text field, falling back to attributedBody text
    // On Tahoe (macOS 26+), outgoing self-messages may have null text column;
    // the actual text is only in the attributedBody typedstream blob.
    let text = match &message.text {
        Some(t) if !t.is_empty() => Some(t.clone()),
        _ => message
            .attributed_body
            .as_deref()
            .and_then(typedstream::extract_text),
    };
    map.insert("text".to_string(), json!(text));

    // attributedBody: decode typedstream blob to JSON
    let attributed_body = message
        .attributed_body
        .as_deref()
        .and_then(typedstream::decode_attributed_body);
    map.insert(
        "attributedBody".to_string(),
        attributed_body.unwrap_or(Value::Null),
    );

    // handle
    let handle_json = message.handle.as_ref().map(|h| serialize_handle(h, false));
    map.insert("handle".to_string(), handle_json.unwrap_or(Value::Null));

    map.insert("handleId".to_string(), json!(message.handle_id));
    map.insert("otherHandle".to_string(), json!(message.other_handle));

    // attachments
    map.insert(
        "attachments".to_string(),
        serialize_attachments(&message.attachments, attachment_config, is_for_notification),
    );

    map.insert("subject".to_string(), json!(message.subject));
    map.insert("error".to_string(), json!(message.error));
    map.insert("dateCreated".to_string(), json!(message.date));
    map.insert("dateRead".to_string(), json!(message.date_read));
    map.insert("dateDelivered".to_string(), json!(message.date_delivered));
    map.insert("isDelivered".to_string(), json!(message.is_delivered));
    map.insert("isFromMe".to_string(), json!(message.is_from_me));
    map.insert("hasDdResults".to_string(), json!(message.has_dd_results));
    map.insert("isArchived".to_string(), json!(message.is_archive));
    map.insert("itemType".to_string(), json!(message.item_type));
    map.insert("groupTitle".to_string(), json!(message.group_title));
    map.insert(
        "groupActionType".to_string(),
        json!(message.group_action_type),
    );
    map.insert(
        "balloonBundleId".to_string(),
        json!(message.balloon_bundle_id),
    );
    map.insert(
        "associatedMessageGuid".to_string(),
        json!(message.associated_message_guid),
    );
    map.insert(
        "associatedMessageType".to_string(),
        json!(message.associated_message_type),
    );
    map.insert(
        "associatedMessageEmoji".to_string(),
        json!(message.associated_message_emoji),
    );
    map.insert(
        "expressiveSendStyleId".to_string(),
        json!(message.expressive_send_style_id),
    );
    map.insert(
        "threadOriginatorGuid".to_string(),
        json!(message.thread_originator_guid),
    );

    // hasPayloadData: boolean indicating if payload_data blob exists
    let has_payload = message.payload_data.is_some();
    map.insert("hasPayloadData".to_string(), json!(has_payload));

    // --- Non-notification fields ---
    if !is_for_notification {
        map.insert("country".to_string(), json!(message.country));
        map.insert("isDelayed".to_string(), json!(message.is_delayed));
        map.insert("isAutoReply".to_string(), json!(message.is_auto_reply));
        map.insert(
            "isSystemMessage".to_string(),
            json!(message.is_system_message),
        );
        map.insert(
            "isServiceMessage".to_string(),
            json!(message.is_service_message),
        );
        map.insert("isForward".to_string(), json!(message.is_forward));
        map.insert(
            "threadOriginatorPart".to_string(),
            json!(message.thread_originator_part),
        );
        map.insert(
            "isCorrupt".to_string(),
            json!(message.is_corrupt.unwrap_or(false)),
        );
        map.insert("datePlayed".to_string(), json!(message.date_played));
        map.insert("cacheRoomnames".to_string(), json!(message.cache_roomnames));
        map.insert(
            "isSpam".to_string(),
            json!(message.is_spam.unwrap_or(false)),
        );

        // Note: isExpired maps to isExpirable (the entity field name)
        map.insert("isExpired".to_string(), json!(message.is_expirable));

        map.insert(
            "timeExpressiveSendPlayed".to_string(),
            json!(message.time_expressive_send_played),
        );
        map.insert(
            "isAudioMessage".to_string(),
            json!(message.is_audio_message),
        );
        map.insert("replyToGuid".to_string(), json!(message.reply_to_guid));
        map.insert("shareStatus".to_string(), json!(message.share_status));
        map.insert("shareDirection".to_string(), json!(message.share_direction));

        map.insert(
            "wasDeliveredQuietly".to_string(),
            json!(message.was_delivered_quietly.unwrap_or(false)),
        );
        map.insert(
            "didNotifyRecipient".to_string(),
            json!(message.did_notify_recipient.unwrap_or(false)),
        );
    }

    // --- Chats (if config.includeChats) ---
    if config.include_chats {
        let chat_config = ChatSerializerConfig {
            include_participants: false,
            include_messages: false,
        };
        map.insert(
            "chats".to_string(),
            serialize_chats(&message.chats, &chat_config, is_for_notification),
        );
    }

    // messageSummaryInfo: binary plist blob with short key renaming
    let msg_summary = message
        .message_summary_info
        .as_deref()
        .and_then(plist_decode::decode_message_plist);
    map.insert(
        "messageSummaryInfo".to_string(),
        msg_summary.unwrap_or(Value::Null),
    );

    // payloadData: binary plist blob with short key renaming
    let payload = message
        .payload_data
        .as_deref()
        .and_then(plist_decode::decode_message_plist);
    map.insert("payloadData".to_string(), payload.unwrap_or(Value::Null));

    map.insert("dateEdited".to_string(), json!(message.date_edited));
    map.insert("dateRetracted".to_string(), json!(message.date_retracted));
    map.insert("partCount".to_string(), json!(message.part_count));

    // --- Post-processing: null out blobs if config says not to parse ---
    if !config.parse_attributed_body
        && let Some(v) = map.get_mut("attributedBody")
    {
        *v = Value::Null;
    }
    if !config.parse_message_summary
        && let Some(v) = map.get_mut("messageSummaryInfo")
    {
        *v = Value::Null;
    }
    if !config.parse_payload_data
        && let Some(v) = map.get_mut("payloadData")
    {
        *v = Value::Null;
    }

    Value::Object(map)
}

/// Serialize a list of Messages to JSON.
pub fn serialize_messages(
    messages: &[Message],
    config: &MessageSerializerConfig,
    attachment_config: &AttachmentSerializerConfig,
    is_for_notification: bool,
) -> Value {
    let list: Vec<Value> = messages
        .iter()
        .map(|m| serialize_message(m, config, attachment_config, is_for_notification))
        .collect();
    Value::Array(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use imessage_db::imessage::entities::Handle;

    fn test_message() -> Message {
        Message {
            rowid: 100,
            guid: "MSG-GUID-1234".to_string(),
            text: Some("Hello, world!".to_string()),
            handle_id: 1,
            handle: Some(Handle {
                rowid: 1,
                id: "+15551234567".to_string(),
                country: Some("us".to_string()),
                service: "iMessage".to_string(),
                uncanonicalized_id: None,
            }),
            date: Some(1700000000000),
            date_read: None,
            date_delivered: Some(1700000001000),
            is_from_me: false,
            is_delivered: true,
            is_archive: false,
            item_type: 0,
            group_action_type: 0,
            has_dd_results: false,
            error: 0,
            ..Default::default()
        }
    }

    #[test]
    fn serialize_basic_message() {
        let config = MessageSerializerConfig {
            include_chats: false,
            ..Default::default()
        };
        let att_config = AttachmentSerializerConfig::default();
        let json = serialize_message(&test_message(), &config, &att_config, false);

        assert_eq!(json["originalROWID"], 100);
        assert_eq!(json["guid"], "MSG-GUID-1234");
        assert_eq!(json["text"], "Hello, world!");
        assert_eq!(json["handleId"], 1);
        assert_eq!(json["isFromMe"], false);
        assert_eq!(json["isDelivered"], true);
        assert_eq!(json["dateCreated"], 1700000000000i64);
        assert_eq!(json["dateDelivered"], 1700000001000i64);
        assert!(json["dateRead"].is_null());
    }

    #[test]
    fn handle_serialized_inline() {
        let config = MessageSerializerConfig::default();
        let att_config = AttachmentSerializerConfig::default();
        let json = serialize_message(&test_message(), &config, &att_config, false);

        assert!(json["handle"].is_object());
        assert_eq!(json["handle"]["address"], "+15551234567");
        assert_eq!(json["handle"]["service"], "iMessage");
    }

    #[test]
    fn null_handle_when_missing() {
        let mut msg = test_message();
        msg.handle = None;
        let config = MessageSerializerConfig::default();
        let att_config = AttachmentSerializerConfig::default();
        let json = serialize_message(&msg, &config, &att_config, false);
        assert!(json["handle"].is_null());
    }

    #[test]
    fn notification_excludes_non_essential_fields() {
        let config = MessageSerializerConfig::default();
        let att_config = AttachmentSerializerConfig::default();
        let json = serialize_message(&test_message(), &config, &att_config, true);

        // Core fields present
        assert!(json.get("originalROWID").is_some());
        assert!(json.get("guid").is_some());
        assert!(json.get("text").is_some());

        // Non-essential fields absent
        assert!(json.get("country").is_none());
        assert!(json.get("isDelayed").is_none());
        assert!(json.get("isAutoReply").is_none());
        assert!(json.get("shareStatus").is_none());
    }

    #[test]
    fn is_expired_maps_to_is_expirable() {
        let mut msg = test_message();
        msg.is_expirable = true;
        let config = MessageSerializerConfig::default();
        let att_config = AttachmentSerializerConfig::default();
        let json = serialize_message(&msg, &config, &att_config, false);
        assert_eq!(json["isExpired"], true);
    }

    #[test]
    fn has_payload_data_flag() {
        let mut msg = test_message();
        msg.payload_data = Some(vec![1, 2, 3]);
        let config = MessageSerializerConfig::default();
        let att_config = AttachmentSerializerConfig::default();
        let json = serialize_message(&msg, &config, &att_config, false);
        assert_eq!(json["hasPayloadData"], true);

        msg.payload_data = None;
        let json = serialize_message(&msg, &config, &att_config, false);
        assert_eq!(json["hasPayloadData"], false);
    }

    #[test]
    fn empty_attachments_array() {
        let config = MessageSerializerConfig::default();
        let att_config = AttachmentSerializerConfig::default();
        let json = serialize_message(&test_message(), &config, &att_config, false);
        assert!(json["attachments"].is_array());
        assert_eq!(json["attachments"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn field_order_core() {
        let config = MessageSerializerConfig {
            include_chats: false,
            ..Default::default()
        };
        let att_config = AttachmentSerializerConfig::default();
        let json = serialize_message(&test_message(), &config, &att_config, true);
        let serialized = serde_json::to_string(&json).unwrap();

        // Verify critical ordering
        let rowid_pos = serialized.find("originalROWID").unwrap();
        let guid_pos = serialized.find("\"guid\"").unwrap();
        let text_pos = serialized.find("\"text\"").unwrap();
        let handle_pos = serialized.find("\"handle\"").unwrap();
        let date_pos = serialized.find("dateCreated").unwrap();

        assert!(rowid_pos < guid_pos);
        assert!(guid_pos < text_pos);
        assert!(text_pos < handle_pos);
        assert!(handle_pos < date_pos);
    }
}
