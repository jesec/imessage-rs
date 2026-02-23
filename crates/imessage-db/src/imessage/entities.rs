/// Entity structs mapping to iMessage chat.db tables.
///
/// These are plain Rust structs — no ORM. Values are transformed
/// during row reading (booleans, dates, reaction types).
///
/// Fields use transformed types:
/// - Boolean columns: `bool` (from 0/1 integers)
/// - Date columns: `Option<i64>` (Unix ms, None if DB value is 0/null)
/// - associated_message_type: `Option<String>` (reaction name or raw int as string)
/// - Blob columns (attributedBody, etc.): `Option<Vec<u8>>` (raw bytes, decoded later)
use serde::Serialize;

/// A message row from the `message` table.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Message {
    // Core columns (always present)
    pub rowid: i64,
    pub guid: String,
    pub text: Option<String>,
    pub replace: i64,
    pub service_center: Option<String>,
    pub handle_id: i64,
    pub subject: Option<String>,
    pub country: Option<String>,
    pub attributed_body: Option<Vec<u8>>,
    pub version: i64,
    pub r#type: i64,
    pub service: Option<String>,
    pub account: Option<String>,
    pub account_guid: Option<String>,
    pub error: i64,
    pub date: Option<i64>,
    pub date_read: Option<i64>,
    pub date_delivered: Option<i64>,
    pub is_delivered: bool,
    pub is_finished: bool,
    pub is_emote: bool,
    pub is_from_me: bool,
    pub is_empty: bool,
    pub is_delayed: bool,
    pub is_auto_reply: bool,
    pub is_prepared: bool,
    pub is_read: bool,
    pub is_system_message: bool,
    pub is_sent: bool,
    pub has_dd_results: bool,
    pub is_service_message: bool,
    pub is_forward: bool,
    pub was_downgraded: bool,
    pub is_archive: bool,
    pub cache_has_attachments: bool,
    pub cache_roomnames: Option<String>,
    pub was_data_detected: bool,
    pub was_deduplicated: bool,
    pub is_audio_message: bool,
    pub is_played: bool,
    pub date_played: Option<i64>,
    pub item_type: i64,
    pub other_handle: i64,
    pub group_title: Option<String>,
    pub group_action_type: i64,
    pub share_status: i64,
    pub share_direction: i64,
    pub is_expirable: bool,
    pub expire_state: bool,
    pub message_action_type: i64,
    pub message_source: i64,

    pub associated_message_guid: Option<String>,
    pub associated_message_type: Option<String>,
    pub associated_message_emoji: Option<String>,
    pub balloon_bundle_id: Option<String>,
    pub payload_data: Option<Vec<u8>>,
    pub expressive_send_style_id: Option<String>,
    pub associated_message_range_location: Option<i64>,
    pub associated_message_range_length: Option<i64>,
    pub time_expressive_send_played: Option<i64>,
    pub message_summary_info: Option<Vec<u8>>,
    pub reply_to_guid: Option<String>,
    pub is_corrupt: Option<bool>,
    pub is_spam: Option<bool>,
    pub thread_originator_guid: Option<String>,
    pub thread_originator_part: Option<String>,
    pub date_retracted: Option<i64>,
    pub date_edited: Option<i64>,
    pub part_count: Option<i64>,
    pub was_delivered_quietly: Option<bool>,
    pub did_notify_recipient: Option<bool>,

    // Relations (populated by joins)
    pub handle: Option<Handle>,
    pub chats: Vec<Chat>,
    pub attachments: Vec<Attachment>,
}

/// A chat row from the `chat` table.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Chat {
    pub rowid: i64,
    pub guid: String,
    pub style: i64,
    pub state: i64,
    pub account_id: Option<String>,
    pub properties: Option<Vec<u8>>,
    pub chat_identifier: Option<String>,
    pub service_name: Option<String>,
    pub room_name: Option<String>,
    pub account_login: Option<String>,
    pub is_archived: bool,
    pub last_read_message_timestamp: Option<i64>,
    pub last_addressed_handle: Option<String>,
    pub display_name: Option<String>,
    pub group_id: Option<String>,
    pub is_filtered: bool,
    pub successful_query: bool,

    // Relations
    pub participants: Vec<Handle>,
    pub messages: Vec<Message>,
}

impl Chat {
    /// Group chats have style == 43 in the chat.db.
    pub fn is_group(&self) -> bool {
        self.style == 43
    }
}

/// A handle row from the `handle` table.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Handle {
    pub rowid: i64,
    /// The `id` column in the DB (serialized as `address` in the API).
    pub id: String,
    pub country: Option<String>,
    pub service: String,
    pub uncanonicalized_id: Option<String>,
}

/// An attachment row from the `attachment` table.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Attachment {
    pub rowid: i64,
    pub guid: String,
    pub created_date: Option<i64>,
    pub start_date: Option<i64>,
    /// The `filename` column in the DB (the file path).
    pub filename: Option<String>,
    pub uti: Option<String>,
    pub mime_type: Option<String>,
    pub transfer_state: i64,
    pub is_outgoing: bool,
    pub user_info: Option<Vec<u8>>,
    pub transfer_name: Option<String>,
    pub total_bytes: i64,

    pub is_sticker: Option<bool>,
    pub sticker_user_info: Option<Vec<u8>>,
    pub attribution_info: Option<Vec<u8>>,
    pub hide_attachment: Option<bool>,
    pub original_guid: Option<String>,
}
