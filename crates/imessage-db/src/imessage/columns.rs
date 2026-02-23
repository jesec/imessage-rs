use rusqlite::Connection;
/// Dynamic column detection for the iMessage database.
///
/// Different macOS versions have different columns in the message, chat,
/// and attachment tables. Instead of hardcoding version checks, we use
/// `PRAGMA table_info(table_name)` to discover available columns at runtime.
///
/// This approach is more robust than version-gating because:
/// 1. It handles edge cases where schema doesn't match expected version
/// 2. It works with any macOS version without needing to know the mapping
/// 3. Columns are conditionally included based on what actually exists
use std::collections::HashSet;

/// Detected columns for a specific table.
#[derive(Debug, Clone)]
pub struct TableColumns {
    columns: HashSet<String>,
}

impl TableColumns {
    /// Query PRAGMA table_info and collect all column names.
    pub fn detect(conn: &Connection, table: &str) -> Self {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({})", table))
            .expect("PRAGMA table_info should always work");

        let columns: HashSet<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("PRAGMA query should succeed")
            .filter_map(|r| r.ok())
            .collect();

        Self { columns }
    }

    /// Check if a column exists in this table.
    pub fn has(&self, col: &str) -> bool {
        self.columns.contains(col)
    }

    /// Get all column names.
    pub fn all(&self) -> &HashSet<String> {
        &self.columns
    }
}

/// Detected schema for the entire iMessage database.
/// Populated once on startup via `PRAGMA table_info`.
#[derive(Debug, Clone)]
pub struct DetectedSchema {
    pub message: TableColumns,
    pub chat: TableColumns,
    pub handle: TableColumns,
    pub attachment: TableColumns,
}

impl DetectedSchema {
    /// Detect all table schemas from the given connection.
    pub fn detect(conn: &Connection) -> Self {
        Self {
            message: TableColumns::detect(conn, "message"),
            chat: TableColumns::detect(conn, "chat"),
            handle: TableColumns::detect(conn, "handle"),
            attachment: TableColumns::detect(conn, "attachment"),
        }
    }

    /// Build the SELECT column list for the message table (71 columns).
    /// All columns are always present on Sequoia+.
    pub fn message_select_columns(&self) -> Vec<&'static str> {
        vec![
            "message.ROWID",
            "message.guid",
            "message.text",
            "message.replace",
            "message.service_center",
            "message.handle_id",
            "message.subject",
            "message.country",
            "message.attributedBody",
            "message.version",
            "message.type",
            "message.service",
            "message.account",
            "message.account_guid",
            "message.error",
            "message.date",
            "message.date_read",
            "message.date_delivered",
            "message.is_delivered",
            "message.is_finished",
            "message.is_emote",
            "message.is_from_me",
            "message.is_empty",
            "message.is_delayed",
            "message.is_auto_reply",
            "message.is_prepared",
            "message.is_read",
            "message.is_system_message",
            "message.is_sent",
            "message.has_dd_results",
            "message.is_service_message",
            "message.is_forward",
            "message.was_downgraded",
            "message.is_archive",
            "message.cache_has_attachments",
            "message.cache_roomnames",
            "message.was_data_detected",
            "message.was_deduplicated",
            "message.is_audio_message",
            "message.is_played",
            "message.date_played",
            "message.item_type",
            "message.other_handle",
            "message.group_title",
            "message.group_action_type",
            "message.share_status",
            "message.share_direction",
            "message.is_expirable",
            "message.expire_state",
            "message.message_action_type",
            "message.message_source",
            "message.associated_message_guid",
            "message.associated_message_type",
            "message.associated_message_emoji",
            "message.balloon_bundle_id",
            "message.payload_data",
            "message.expressive_send_style_id",
            "message.associated_message_range_location",
            "message.associated_message_range_length",
            "message.time_expressive_send_played",
            "message.message_summary_info",
            "message.reply_to_guid",
            "message.is_corrupt",
            "message.is_spam",
            "message.thread_originator_guid",
            "message.thread_originator_part",
            "message.was_delivered_quietly",
            "message.did_notify_recipient",
            "message.date_retracted",
            "message.date_edited",
            "message.part_count",
        ]
    }

    /// Build the SELECT column list for the chat table (16 columns).
    /// All columns are always present on Sequoia+.
    pub fn chat_select_columns(&self) -> Vec<&'static str> {
        vec![
            "chat.ROWID",
            "chat.guid",
            "chat.style",
            "chat.state",
            "chat.account_id",
            "chat.properties",
            "chat.chat_identifier",
            "chat.service_name",
            "chat.room_name",
            "chat.account_login",
            "chat.is_archived",
            "chat.last_addressed_handle",
            "chat.display_name",
            "chat.group_id",
            "chat.is_filtered",
            "chat.successful_query",
            "chat.last_read_message_timestamp",
        ]
    }

    /// Build the SELECT column list for the attachment table (17 columns).
    /// All columns are always present on Sequoia+.
    pub fn attachment_select_columns(&self) -> Vec<&'static str> {
        vec![
            "attachment.ROWID",
            "attachment.guid",
            "attachment.created_date",
            "attachment.start_date",
            "attachment.filename",
            "attachment.uti",
            "attachment.mime_type",
            "attachment.transfer_state",
            "attachment.is_outgoing",
            "attachment.user_info",
            "attachment.transfer_name",
            "attachment.total_bytes",
            "attachment.is_sticker",
            "attachment.sticker_user_info",
            "attachment.attribution_info",
            "attachment.hide_attachment",
            "attachment.original_guid",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_columns_has() {
        let mut set = HashSet::new();
        set.insert("guid".to_string());
        set.insert("text".to_string());
        let tc = TableColumns { columns: set };
        assert!(tc.has("guid"));
        assert!(tc.has("text"));
        assert!(!tc.has("nonexistent"));
    }

    #[test]
    fn column_counts() {
        let empty = TableColumns {
            columns: HashSet::new(),
        };
        let schema = DetectedSchema {
            message: empty.clone(),
            chat: empty.clone(),
            handle: empty.clone(),
            attachment: empty,
        };
        assert_eq!(schema.message_select_columns().len(), 71);
        assert_eq!(schema.chat_select_columns().len(), 17);
        assert_eq!(schema.attachment_select_columns().len(), 17);
    }
}
