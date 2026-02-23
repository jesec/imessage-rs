/// Functions to read entity structs from rusqlite rows.
///
/// Each function reads columns by name from a Row, applying transformers
/// as needed.
use rusqlite::Row;

use super::columns::DetectedSchema;
use super::entities::{Attachment, Chat, Handle, Message};
use super::transformers::{bool_from_db, date_from_db, reaction_type_from_db};

/// Helper to safely get an optional i64 column.
fn get_opt_i64(row: &Row, col: &str) -> Option<i64> {
    row.get::<_, Option<i64>>(col).unwrap_or(None)
}

/// Helper to safely get an optional String column.
fn get_opt_str(row: &Row, col: &str) -> Option<String> {
    row.get::<_, Option<String>>(col).unwrap_or(None)
}

/// Helper to safely get an optional blob column.
fn get_opt_blob(row: &Row, col: &str) -> Option<Vec<u8>> {
    row.get::<_, Option<Vec<u8>>>(col).unwrap_or(None)
}

/// Helper to get an i64 column with default 0.
fn get_i64(row: &Row, col: &str) -> i64 {
    row.get::<_, i64>(col).unwrap_or(0)
}

/// Helper to get a bool column (integer 0/1 → bool).
fn get_bool(row: &Row, col: &str) -> bool {
    bool_from_db(get_i64(row, col))
}

/// Helper to get a date column (Apple timestamp → Unix ms).
fn get_date(row: &Row, col: &str) -> Option<i64> {
    let raw = get_i64(row, col);
    date_from_db(raw)
}

/// Read a Message entity from a row, using the detected schema to know which columns exist.
/// The row must have been SELECTed with the columns from `schema.message_select_columns()`.
///
/// Column names use the `message.col_name` alias pattern from the SQL query.
/// However, rusqlite returns columns as they appear in the SELECT — we use
/// unqualified names since that's what rusqlite resolves.
///
/// All 71 columns are always present on Sequoia+.
pub fn read_message(row: &Row, _schema: &DetectedSchema) -> Message {
    let raw_type = get_i64(row, "associated_message_type");

    Message {
        rowid: get_i64(row, "ROWID"),
        guid: get_opt_str(row, "guid").unwrap_or_default(),
        text: get_opt_str(row, "text"),
        replace: get_i64(row, "replace"),
        service_center: get_opt_str(row, "service_center"),
        handle_id: get_i64(row, "handle_id"),
        subject: get_opt_str(row, "subject"),
        country: get_opt_str(row, "country"),
        attributed_body: get_opt_blob(row, "attributedBody"),
        version: get_i64(row, "version"),
        r#type: get_i64(row, "type"),
        service: get_opt_str(row, "service"),
        account: get_opt_str(row, "account"),
        account_guid: get_opt_str(row, "account_guid"),
        error: get_i64(row, "error"),
        date: get_date(row, "date"),
        date_read: get_date(row, "date_read"),
        date_delivered: get_date(row, "date_delivered"),
        is_delivered: get_bool(row, "is_delivered"),
        is_finished: get_bool(row, "is_finished"),
        is_emote: get_bool(row, "is_emote"),
        is_from_me: get_bool(row, "is_from_me"),
        is_empty: get_bool(row, "is_empty"),
        is_delayed: get_bool(row, "is_delayed"),
        is_auto_reply: get_bool(row, "is_auto_reply"),
        is_prepared: get_bool(row, "is_prepared"),
        is_read: get_bool(row, "is_read"),
        is_system_message: get_bool(row, "is_system_message"),
        is_sent: get_bool(row, "is_sent"),
        has_dd_results: get_bool(row, "has_dd_results"),
        is_service_message: get_bool(row, "is_service_message"),
        is_forward: get_bool(row, "is_forward"),
        was_downgraded: get_bool(row, "was_downgraded"),
        is_archive: get_bool(row, "is_archive"),
        cache_has_attachments: get_bool(row, "cache_has_attachments"),
        cache_roomnames: get_opt_str(row, "cache_roomnames"),
        was_data_detected: get_bool(row, "was_data_detected"),
        was_deduplicated: get_bool(row, "was_deduplicated"),
        is_audio_message: get_bool(row, "is_audio_message"),
        is_played: get_bool(row, "is_played"),
        date_played: get_date(row, "date_played"),
        item_type: get_i64(row, "item_type"),
        other_handle: get_i64(row, "other_handle"),
        group_title: get_opt_str(row, "group_title"),
        group_action_type: get_i64(row, "group_action_type"),
        share_status: get_i64(row, "share_status"),
        share_direction: get_i64(row, "share_direction"),
        is_expirable: get_bool(row, "is_expirable"),
        expire_state: get_bool(row, "expire_state"),
        message_action_type: get_i64(row, "message_action_type"),
        message_source: get_i64(row, "message_source"),
        associated_message_guid: get_opt_str(row, "associated_message_guid"),
        associated_message_type: reaction_type_from_db(raw_type),
        associated_message_emoji: get_opt_str(row, "associated_message_emoji"),
        balloon_bundle_id: get_opt_str(row, "balloon_bundle_id"),
        payload_data: get_opt_blob(row, "payload_data"),
        expressive_send_style_id: get_opt_str(row, "expressive_send_style_id"),
        associated_message_range_location: Some(get_i64(row, "associated_message_range_location")),
        associated_message_range_length: Some(get_i64(row, "associated_message_range_length")),
        time_expressive_send_played: get_date(row, "time_expressive_send_played"),
        message_summary_info: get_opt_blob(row, "message_summary_info"),
        reply_to_guid: get_opt_str(row, "reply_to_guid"),
        is_corrupt: Some(get_bool(row, "is_corrupt")),
        is_spam: Some(get_bool(row, "is_spam")),
        thread_originator_guid: get_opt_str(row, "thread_originator_guid"),
        thread_originator_part: get_opt_str(row, "thread_originator_part"),
        was_delivered_quietly: Some(get_bool(row, "was_delivered_quietly")),
        did_notify_recipient: Some(get_bool(row, "did_notify_recipient")),
        date_retracted: get_date(row, "date_retracted"),
        date_edited: get_date(row, "date_edited"),
        part_count: get_opt_i64(row, "part_count"),
        ..Default::default()
    }
}

/// Read a Chat entity from a row.
/// All 17 columns are always present on Sequoia+.
pub fn read_chat(row: &Row, _schema: &DetectedSchema) -> Chat {
    Chat {
        rowid: get_i64(row, "ROWID"),
        guid: get_opt_str(row, "guid").unwrap_or_default(),
        style: get_i64(row, "style"),
        state: get_i64(row, "state"),
        account_id: get_opt_str(row, "account_id"),
        properties: get_opt_blob(row, "properties"),
        chat_identifier: get_opt_str(row, "chat_identifier"),
        service_name: get_opt_str(row, "service_name"),
        room_name: get_opt_str(row, "room_name"),
        account_login: get_opt_str(row, "account_login"),
        is_archived: get_bool(row, "is_archived"),
        last_addressed_handle: get_opt_str(row, "last_addressed_handle"),
        display_name: get_opt_str(row, "display_name"),
        group_id: get_opt_str(row, "group_id"),
        is_filtered: get_bool(row, "is_filtered"),
        successful_query: get_bool(row, "successful_query"),
        last_read_message_timestamp: get_date(row, "last_read_message_timestamp"),
        ..Default::default()
    }
}

/// Read a Handle entity from a row.
pub fn read_handle(row: &Row) -> Handle {
    Handle {
        rowid: get_i64(row, "ROWID"),
        id: get_opt_str(row, "id").unwrap_or_default(),
        country: get_opt_str(row, "country"),
        service: get_opt_str(row, "service").unwrap_or_else(|| "iMessage".to_string()),
        uncanonicalized_id: get_opt_str(row, "uncanonicalized_id"),
    }
}

/// Read an Attachment entity from a row.
/// All 17 columns are always present on Sequoia+.
pub fn read_attachment(row: &Row, _schema: &DetectedSchema) -> Attachment {
    Attachment {
        rowid: get_i64(row, "ROWID"),
        guid: get_opt_str(row, "guid").unwrap_or_default(),
        created_date: get_date(row, "created_date"),
        start_date: get_date(row, "start_date"),
        filename: get_opt_str(row, "filename"),
        uti: get_opt_str(row, "uti"),
        mime_type: get_opt_str(row, "mime_type"),
        transfer_state: get_i64(row, "transfer_state"),
        is_outgoing: get_bool(row, "is_outgoing"),
        user_info: get_opt_blob(row, "user_info"),
        transfer_name: get_opt_str(row, "transfer_name"),
        total_bytes: get_i64(row, "total_bytes"),
        is_sticker: Some(get_bool(row, "is_sticker")),
        sticker_user_info: get_opt_blob(row, "sticker_user_info"),
        attribution_info: get_opt_blob(row, "attribution_info"),
        hide_attachment: Some(get_bool(row, "hide_attachment")),
        original_guid: get_opt_str(row, "original_guid"),
    }
}

/// Read a Handle from a row that has handle columns with a prefix (e.g., from a JOIN).
/// Used when reading handles embedded in message queries.
pub fn read_handle_from_join(row: &Row, prefix: &str) -> Option<Handle> {
    let rowid_col = format!("{prefix}ROWID");
    let id_col = format!("{prefix}id");
    let country_col = format!("{prefix}country");
    let service_col = format!("{prefix}service");
    let uncanonicalized_col = format!("{prefix}uncanonicalized_id");

    let rowid = row.get::<_, Option<i64>>(&*rowid_col).unwrap_or(None)?;
    Some(Handle {
        rowid,
        id: row
            .get::<_, Option<String>>(&*id_col)
            .unwrap_or(None)
            .unwrap_or_default(),
        country: row.get::<_, Option<String>>(&*country_col).unwrap_or(None),
        service: row
            .get::<_, Option<String>>(&*service_col)
            .unwrap_or(None)
            .unwrap_or_else(|| "iMessage".to_string()),
        uncanonicalized_id: row
            .get::<_, Option<String>>(&*uncanonicalized_col)
            .unwrap_or(None),
    })
}
