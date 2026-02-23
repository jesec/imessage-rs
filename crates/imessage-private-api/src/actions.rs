/// All outgoing action payloads for the Private API helper dylib.
///
/// Each action maps to a JSON object sent over TCP with an action string.
use serde_json::{Value, json};

use crate::transaction::TransactionType;

/// An action to send to the helper dylib.
pub struct Action {
    pub name: &'static str,
    pub data: Value,
    pub transaction_type: Option<TransactionType>,
}

/// Common optional fields shared across send actions (text, multipart, attachment).
#[derive(Debug, Default)]
pub struct SendOptions<'a> {
    pub subject: Option<&'a str>,
    pub effect_id: Option<&'a str>,
    pub selected_message_guid: Option<&'a str>,
    pub part_index: Option<i64>,
    pub attributed_body: Option<&'a Value>,
}

/// Apply common SendOptions fields to a JSON data object.
fn apply_send_options(data: &mut Value, opts: &SendOptions) {
    if let Some(s) = opts.subject {
        data["subject"] = json!(s);
    }
    if let Some(e) = opts.effect_id {
        data["effectId"] = json!(e);
    }
    if let Some(g) = opts.selected_message_guid {
        data["selectedMessageGuid"] = json!(g);
    }
    if let Some(ab) = opts.attributed_body {
        data["attributedBody"] = ab.clone();
    }
}

// ---------------------------------------------------------------------------
// Message actions (PrivateApiMessage)
// ---------------------------------------------------------------------------

pub fn send_message(
    chat_guid: &str,
    message: &str,
    opts: &SendOptions,
    text_formatting: Option<&Value>,
    dd_scan: Option<bool>,
) -> Action {
    // Always send all fields (null for unset optionals, 0 for partIndex)
    let mut data = json!({
        "chatGuid": chat_guid,
        "message": message,
        "subject": Value::Null,
        "attributedBody": Value::Null,
        "effectId": Value::Null,
        "selectedMessageGuid": Value::Null,
        "partIndex": opts.part_index.unwrap_or(0),
        "textFormatting": Value::Null,
    });
    apply_send_options(&mut data, opts);
    if let Some(tf) = text_formatting {
        data["textFormatting"] = tf.clone();
    }
    if let Some(dd) = dd_scan {
        data["ddScan"] = json!(if dd { 1 } else { 0 });
    }

    Action {
        name: "send-message",
        data,
        transaction_type: Some(TransactionType::Message),
    }
}

pub fn send_multipart(
    chat_guid: &str,
    parts: &Value,
    opts: &SendOptions,
    dd_scan: Option<bool>,
) -> Action {
    // Always send all fields (null for unset optionals, 0 for partIndex)
    let mut data = json!({
        "chatGuid": chat_guid,
        "parts": parts,
        "subject": Value::Null,
        "effectId": Value::Null,
        "selectedMessageGuid": Value::Null,
        "partIndex": opts.part_index.unwrap_or(0),
        "attributedBody": Value::Null,
    });
    apply_send_options(&mut data, opts);
    if let Some(dd) = dd_scan {
        data["ddScan"] = json!(if dd { 1 } else { 0 });
    }

    Action {
        name: "send-multipart",
        data,
        transaction_type: Some(TransactionType::Message),
    }
}

pub fn send_reaction(
    chat_guid: &str,
    selected_message_guid: &str,
    reaction_type: &str,
    part_index: Option<i64>,
    emoji: Option<&str>,
    sticker_path: Option<&str>,
) -> Action {
    // Always send partIndex (default 0)
    let mut data = json!({
        "chatGuid": chat_guid,
        "selectedMessageGuid": selected_message_guid,
        "reactionType": reaction_type,
        "partIndex": part_index.unwrap_or(0),
    });
    if let Some(e) = emoji {
        data["emoji"] = json!(e);
    }
    if let Some(sp) = sticker_path {
        data["stickerPath"] = json!(sp);
    }

    Action {
        name: "send-reaction",
        data,
        transaction_type: Some(TransactionType::Message),
    }
}

pub fn edit_message(
    chat_guid: &str,
    message_guid: &str,
    edited_message: &str,
    backwards_compat_message: &str,
    part_index: Option<i64>,
) -> Action {
    // Always send partIndex (default 0)
    let data = json!({
        "chatGuid": chat_guid,
        "messageGuid": message_guid,
        "editedMessage": edited_message,
        "backwardsCompatibilityMessage": backwards_compat_message,
        "partIndex": part_index.unwrap_or(0),
    });

    Action {
        name: "edit-message",
        data,
        transaction_type: Some(TransactionType::Message),
    }
}

pub fn unsend_message(chat_guid: &str, message_guid: &str, part_index: Option<i64>) -> Action {
    // Always send partIndex (default 0)
    let data = json!({
        "chatGuid": chat_guid,
        "messageGuid": message_guid,
        "partIndex": part_index.unwrap_or(0),
    });

    Action {
        name: "unsend-message",
        data,
        transaction_type: Some(TransactionType::Message),
    }
}

pub fn get_embedded_media(chat_guid: &str, message_guid: &str) -> Action {
    Action {
        name: "balloon-bundle-media-path",
        data: json!({
            "chatGuid": chat_guid,
            "messageGuid": message_guid,
        }),
        transaction_type: Some(TransactionType::Message),
    }
}

pub fn notify_silenced(chat_guid: &str, message_guid: &str) -> Action {
    Action {
        name: "notify-anyways",
        data: json!({
            "chatGuid": chat_guid,
            "messageGuid": message_guid,
        }),
        transaction_type: Some(TransactionType::Message),
    }
}

pub fn search_messages(query: &str, match_type: &str) -> Action {
    Action {
        name: "search-messages",
        data: json!({
            "query": query,
            "matchType": match_type,
        }),
        transaction_type: Some(TransactionType::Message),
    }
}

// ---------------------------------------------------------------------------
// Chat actions (PrivateApiChat)
// ---------------------------------------------------------------------------

pub fn create_chat(
    addresses: &[String],
    message: &str,
    service: &str,
    attributed_body: Option<&Value>,
    effect_id: Option<&str>,
    subject: Option<&str>,
) -> Action {
    let mut data = json!({
        "addresses": addresses,
        "message": message,
        "service": service,
    });
    if let Some(ab) = attributed_body {
        data["attributedBody"] = ab.clone();
    }
    if let Some(e) = effect_id {
        data["effectId"] = json!(e);
    }
    if let Some(s) = subject {
        data["subject"] = json!(s);
    }

    Action {
        name: "create-chat",
        data,
        transaction_type: Some(TransactionType::Message), // returns message GUID
    }
}

pub fn delete_message(chat_guid: &str, message_guid: &str) -> Action {
    Action {
        name: "delete-message",
        data: json!({
            "chatGuid": chat_guid,
            "messageGuid": message_guid,
        }),
        transaction_type: Some(TransactionType::Chat),
    }
}

pub fn start_typing(chat_guid: &str) -> Action {
    Action {
        name: "start-typing",
        data: json!({ "chatGuid": chat_guid }),
        transaction_type: None, // fire-and-forget
    }
}

pub fn stop_typing(chat_guid: &str) -> Action {
    Action {
        name: "stop-typing",
        data: json!({ "chatGuid": chat_guid }),
        transaction_type: None,
    }
}

pub fn mark_chat_read(chat_guid: &str) -> Action {
    Action {
        name: "mark-chat-read",
        data: json!({ "chatGuid": chat_guid }),
        transaction_type: None,
    }
}

pub fn mark_chat_unread(chat_guid: &str) -> Action {
    Action {
        name: "mark-chat-unread",
        data: json!({ "chatGuid": chat_guid }),
        transaction_type: None,
    }
}

pub fn add_participant(chat_guid: &str, address: &str) -> Action {
    Action {
        name: "add-participant",
        data: json!({
            "chatGuid": chat_guid,
            "address": address,
        }),
        transaction_type: Some(TransactionType::Chat),
    }
}

pub fn remove_participant(chat_guid: &str, address: &str) -> Action {
    Action {
        name: "remove-participant",
        data: json!({
            "chatGuid": chat_guid,
            "address": address,
        }),
        transaction_type: Some(TransactionType::Chat),
    }
}

pub fn set_display_name(chat_guid: &str, new_name: &str) -> Action {
    Action {
        name: "set-display-name",
        data: json!({
            "chatGuid": chat_guid,
            "newName": new_name,
        }),
        transaction_type: Some(TransactionType::Chat),
    }
}

pub fn set_group_chat_icon(chat_guid: &str, file_path: Option<&str>) -> Action {
    Action {
        name: "update-group-photo",
        data: json!({
            "chatGuid": chat_guid,
            "filePath": file_path,
        }),
        transaction_type: Some(TransactionType::Chat),
    }
}

pub fn should_offer_contact_sharing(chat_guid: &str) -> Action {
    Action {
        name: "should-offer-nickname-sharing",
        data: json!({ "chatGuid": chat_guid }),
        transaction_type: Some(TransactionType::Other),
    }
}

pub fn share_contact_card(chat_guid: &str) -> Action {
    Action {
        name: "share-nickname",
        data: json!({ "chatGuid": chat_guid }),
        transaction_type: None,
    }
}

pub fn leave_chat(chat_guid: &str) -> Action {
    Action {
        name: "leave-chat",
        data: json!({ "chatGuid": chat_guid }),
        transaction_type: Some(TransactionType::Chat),
    }
}

pub fn delete_chat(chat_guid: &str) -> Action {
    Action {
        name: "delete-chat",
        data: json!({ "chatGuid": chat_guid }),
        transaction_type: Some(TransactionType::Chat),
    }
}

// ---------------------------------------------------------------------------
// Handle actions (PrivateApiHandle)
// ---------------------------------------------------------------------------

pub fn get_focus_status(address: &str) -> Action {
    Action {
        name: "check-focus-status",
        data: json!({ "address": address }),
        transaction_type: Some(TransactionType::Handle),
    }
}

pub fn get_imessage_availability(address: &str) -> Action {
    let alias_type = if address.contains('@') {
        "email"
    } else {
        "phone"
    };
    Action {
        name: "check-imessage-availability",
        data: json!({
            "aliasType": alias_type,
            "address": address,
        }),
        transaction_type: Some(TransactionType::Handle),
    }
}

pub fn get_facetime_availability(address: &str) -> Action {
    let alias_type = if address.contains('@') {
        "email"
    } else {
        "phone"
    };
    Action {
        name: "check-facetime-availability",
        data: json!({
            "aliasType": alias_type,
            "address": address,
        }),
        transaction_type: Some(TransactionType::Handle),
    }
}

// ---------------------------------------------------------------------------
// Attachment actions (PrivateApiAttachment)
// ---------------------------------------------------------------------------

pub fn send_attachment(
    chat_guid: &str,
    file_path: &str,
    is_audio_message: bool,
    opts: &SendOptions,
) -> Action {
    let mut data = json!({
        "chatGuid": chat_guid,
        "filePath": file_path,
        "isAudioMessage": if is_audio_message { 1 } else { 0 },
        // Always send partIndex (default 0), attributedBody,
        // subject, effectId, selectedMessageGuid
        "partIndex": opts.part_index.unwrap_or(0),
        "attributedBody": Value::Null,
        "subject": Value::Null,
        "effectId": Value::Null,
        "selectedMessageGuid": Value::Null,
    });
    apply_send_options(&mut data, opts);

    Action {
        name: "send-attachment",
        data,
        transaction_type: Some(TransactionType::Attachment),
    }
}

pub fn download_purged_attachment(attachment_guid: &str) -> Action {
    Action {
        name: "download-purged-attachment",
        data: json!({ "attachmentGuid": attachment_guid }),
        transaction_type: None,
    }
}

// ---------------------------------------------------------------------------
// FindMy actions
// ---------------------------------------------------------------------------

pub fn refresh_findmy_friends() -> Action {
    Action {
        name: "refresh-findmy-friends",
        data: Value::Null,
        transaction_type: Some(TransactionType::FindMy),
    }
}

pub fn get_findmy_key() -> Action {
    Action {
        name: "get-findmy-key",
        data: Value::Null,
        transaction_type: Some(TransactionType::FindMy),
    }
}

// ---------------------------------------------------------------------------
// Cloud / iCloud actions
// ---------------------------------------------------------------------------

pub fn get_account_info() -> Action {
    Action {
        name: "get-account-info",
        data: Value::Null,
        transaction_type: Some(TransactionType::Other),
    }
}

pub fn get_contact_card(address: &str) -> Action {
    Action {
        name: "get-nickname-info",
        data: json!({ "address": address }),
        transaction_type: Some(TransactionType::Other),
    }
}

pub fn modify_active_alias(alias: &str) -> Action {
    Action {
        name: "modify-active-alias",
        data: json!({ "alias": alias }),
        transaction_type: Some(TransactionType::Other),
    }
}

// ---------------------------------------------------------------------------
// FaceTime actions
// ---------------------------------------------------------------------------

pub fn answer_call(call_uuid: &str) -> Action {
    Action {
        name: "answer-call",
        data: json!({ "callUUID": call_uuid }),
        transaction_type: Some(TransactionType::Other),
    }
}

pub fn leave_call(call_uuid: &str) -> Action {
    Action {
        name: "leave-call",
        data: json!({ "callUUID": call_uuid }),
        transaction_type: None,
    }
}

/// Generate a FaceTime link.
/// Pass `None` for a new link (no existing call), or `Some(uuid)` for an existing call.
/// The dylib checks `callUUID != [NSNull null]` — sending a non-null string that doesn't
/// match any active call causes a nil-dereference crash. Always send null for new sessions.
pub fn generate_facetime_link(call_uuid: Option<&str>) -> Action {
    Action {
        name: "generate-link",
        data: json!({ "callUUID": call_uuid }),
        transaction_type: Some(TransactionType::Other),
    }
}

pub fn check_typing_status(chat_guid: &str) -> Action {
    Action {
        name: "check-typing-status",
        data: json!({ "chatGuid": chat_guid }),
        transaction_type: Some(TransactionType::Chat),
    }
}

pub fn admit_participant(conversation_uuid: &str, handle_uuid: &str) -> Action {
    Action {
        name: "admit-pending-member",
        data: json!({
            "conversationUUID": conversation_uuid,
            "handleUUID": handle_uuid,
        }),
        transaction_type: Some(TransactionType::Other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_message_action_shape() {
        let action = send_message(
            "iMessage;-;+1555",
            "hello",
            &SendOptions::default(),
            None,
            None,
        );
        assert_eq!(action.name, "send-message");
        assert_eq!(action.data["chatGuid"], "iMessage;-;+1555");
        assert_eq!(action.data["message"], "hello");
        assert_eq!(action.transaction_type, Some(TransactionType::Message));
    }

    #[test]
    fn create_chat_uses_message_transaction() {
        let action = create_chat(&["addr".to_string()], "hi", "iMessage", None, None, None);
        assert_eq!(action.name, "create-chat");
        assert_eq!(action.transaction_type, Some(TransactionType::Message));
    }

    #[test]
    fn fire_and_forget_actions_have_no_transaction() {
        let a = start_typing("guid");
        assert!(a.transaction_type.is_none());
        let b = stop_typing("guid");
        assert!(b.transaction_type.is_none());
        let c = mark_chat_read("guid");
        assert!(c.transaction_type.is_none());
    }

    #[test]
    fn imessage_availability_detects_email() {
        let a = get_imessage_availability("user@icloud.com");
        assert_eq!(a.data["aliasType"], "email");
        let b = get_imessage_availability("+15551234567");
        assert_eq!(b.data["aliasType"], "phone");
    }

    #[test]
    fn send_attachment_encodes_audio_as_int() {
        let a = send_attachment("guid", "/path", true, &SendOptions::default());
        assert_eq!(a.data["isAudioMessage"], 1);
        assert_eq!(a.data["partIndex"], 0); // default partIndex
        let b = send_attachment("guid", "/path", false, &SendOptions::default());
        assert_eq!(b.data["isAudioMessage"], 0);
    }

    #[test]
    fn dd_scan_encodes_as_int() {
        let a = send_message("g", "m", &SendOptions::default(), None, Some(true));
        assert_eq!(a.data["ddScan"], 1);
    }
}
