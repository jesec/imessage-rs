/// MessageRepository: read-only access to the iMessage chat.db database.
///
/// It uses rusqlite directly with hand-built SQL, reading from
/// ~/Library/Messages/chat.db in read-only mode.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;
use tracing::info;

use super::columns::DetectedSchema;
use super::entities::{Attachment, Chat, Handle, Message};
use super::row_reader;
use super::transformers::date_to_db;
use super::types::*;

/// The iMessage database repository.
pub struct MessageRepository {
    conn: Connection,
    schema: Arc<DetectedSchema>,
}

/// Convert a serde_json Value to a rusqlite-bindable parameter.
fn json_to_sql(val: &Value) -> Box<dyn rusqlite::types::ToSql> {
    match val {
        Value::String(s) => Box::new(s.clone()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Box::new(i)
            } else if let Some(f) = n.as_f64() {
                Box::new(f)
            } else {
                Box::new(n.to_string())
            }
        }
        Value::Bool(b) => Box::new(if *b { 1i64 } else { 0i64 }),
        Value::Null => Box::new(Option::<String>::None),
        _ => Box::new(val.to_string()),
    }
}

/// Expand named parameters (`:name`, `:...name`) in a SQL fragment and push bound values.
fn expand_named_params(
    statement: &str,
    args: &HashMap<String, Value>,
    bind_values: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> String {
    let mut result = String::with_capacity(statement.len());
    let chars: Vec<char> = statement.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == ':' {
            let is_spread =
                i + 3 < len && chars[i + 1] == '.' && chars[i + 2] == '.' && chars[i + 3] == '.';
            let name_start = if is_spread { i + 4 } else { i + 1 };

            let mut name_end = name_start;
            while name_end < len && (chars[name_end].is_alphanumeric() || chars[name_end] == '_') {
                name_end += 1;
            }

            if name_end > name_start {
                let name: String = chars[name_start..name_end].iter().collect();
                if let Some(val) = args.get(&name) {
                    if is_spread {
                        if let Some(arr) = val.as_array() {
                            for (j, item) in arr.iter().enumerate() {
                                if j > 0 {
                                    result.push_str(", ");
                                }
                                result.push('?');
                                bind_values.push(json_to_sql(item));
                            }
                        }
                    } else {
                        result.push('?');
                        bind_values.push(json_to_sql(val));
                    }
                    i = name_end;
                    continue;
                }
            }
        }
        result.push(chars[i]);
        i += 1;
    }

    result
}

/// Apply custom WHERE clauses to a SQL query.
fn apply_where_clauses(
    sql: &mut String,
    bind_values: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    where_clauses: &[WhereClause],
) {
    for clause in where_clauses {
        let expanded = expand_named_params(&clause.statement, &clause.args, bind_values);
        sql.push_str(&format!(" AND ({expanded})"));
    }
}

impl MessageRepository {
    /// Open the iMessage database in read-only mode and detect its schema.
    pub fn open(db_path: PathBuf) -> Result<Self> {
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("Failed to open iMessage DB at {}", db_path.display()))?;

        let schema = Arc::new(DetectedSchema::detect(&conn));
        info!(
            "iMessage DB opened: {} message columns detected",
            schema.message_select_columns().len()
        );

        Ok(Self { conn, schema })
    }

    /// Get the detected schema.
    pub fn schema(&self) -> &DetectedSchema {
        &self.schema
    }

    // =========================================================================
    // Counts
    // =========================================================================

    /// Count all messages, with optional filters.
    pub fn get_message_count(&self, params: &MessageCountParams) -> Result<i64> {
        let mut sql = String::from("SELECT COUNT(*) FROM message");
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];

        // Join to chat if filtering by chat_guid
        if let Some(ref guid) = params.chat_guid {
            sql.push_str(
                " INNER JOIN chat_message_join ON message.ROWID = chat_message_join.message_id \
                 INNER JOIN chat ON chat.ROWID = chat_message_join.chat_id",
            );
            sql.push_str(" WHERE chat.guid = ?");
            bind_values.push(Box::new(guid.clone()));
        } else {
            sql.push_str(" WHERE 1=1");
        }

        if params.is_from_me {
            sql.push_str(" AND message.is_from_me = 1");
        }

        if let Some(min) = params.min_row_id {
            sql.push_str(" AND message.ROWID >= ?");
            bind_values.push(Box::new(min));
        }

        if let Some(max) = params.max_row_id {
            sql.push_str(" AND message.ROWID <= ?");
            bind_values.push(Box::new(max));
        }

        // Date filters
        if params.after.is_some() || params.before.is_some() {
            if params.updated {
                self.append_update_date_sql(
                    &mut sql,
                    &mut bind_values,
                    params.after,
                    params.before,
                    false,
                );
            } else {
                self.append_date_sql(&mut sql, &mut bind_values, params.after, params.before);
            }
        }

        // Custom WHERE clauses
        apply_where_clauses(&mut sql, &mut bind_values, &params.where_clauses);

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let count: i64 = self
            .conn
            .query_row(&sql, bind_refs.as_slice(), |row| row.get(0))?;
        Ok(count)
    }

    /// Count all chats.
    pub fn get_chat_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM chat", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Count all handles, optionally filtered by address.
    pub fn get_handle_count(&self, address: Option<&str>) -> Result<i64> {
        if let Some(addr) = address {
            let stripped = addr.replace('+', "");
            let count: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM handle WHERE handle.id LIKE ?",
                [format!("%{stripped}")],
                |row| row.get(0),
            )?;
            Ok(count)
        } else {
            let count: i64 = self
                .conn
                .query_row("SELECT COUNT(*) FROM handle", [], |row| row.get(0))?;
            Ok(count)
        }
    }

    /// Count all attachments.
    pub fn get_attachment_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM attachment", [], |row| row.get(0))?;
        Ok(count)
    }

    // =========================================================================
    // Single-row lookups
    // =========================================================================

    /// Get a single message by GUID, optionally with chats and attachments.
    pub fn get_message(
        &self,
        guid: &str,
        with_chats: bool,
        with_attachments: bool,
    ) -> Result<Option<Message>> {
        let msg_cols = self.schema.message_select_columns();
        let msg_select = msg_cols.join(", ");

        let sql = format!(
            "SELECT {msg_select}, \
             handle.ROWID AS h_ROWID, handle.id AS h_id, handle.country AS h_country, \
             handle.service AS h_service, handle.uncanonicalized_id AS h_uncanonicalized_id \
             FROM message \
             LEFT JOIN handle ON message.handle_id = handle.ROWID \
             WHERE message.guid = ?"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let result = stmt.query_row(params![guid], |row| {
            let mut msg = row_reader::read_message(row, &self.schema);
            msg.handle = row_reader::read_handle_from_join(row, "h_");
            Ok(msg)
        });

        let mut msg = match result {
            Ok(msg) => msg,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        // Fetch chats separately to avoid column name ambiguity from JOIN
        // (both message and chat tables have ROWID, guid, etc.)
        if with_chats {
            msg.chats = self.get_chats_for_message(msg.rowid)?;
        }

        // Fetch attachments separately if requested
        if with_attachments {
            msg.attachments = self.get_attachments_for_message(msg.rowid)?;
        }

        Ok(Some(msg))
    }

    /// Get a single attachment by GUID (handles prefix stripping).
    pub fn get_attachment(&self, attachment_guid: &str) -> Result<Option<Attachment>> {
        let att_cols = self.schema.attachment_select_columns();
        let att_select = att_cols.join(", ");

        // Build lookup GUIDs: original + last 36 chars (strip prefix like "at_x_" or "p:/")
        let mut lookup_guids = vec![attachment_guid.to_string()];
        if attachment_guid.len() > 36 {
            lookup_guids.push(attachment_guid[attachment_guid.len() - 36..].to_string());
        }

        for lookup_guid in &lookup_guids {
            let like_pattern = format!("%{lookup_guid}");

            let sql = format!(
                "SELECT {att_select} FROM attachment \
                 WHERE attachment.original_guid LIKE ?1 OR attachment.guid LIKE ?1 \
                 LIMIT 1"
            );

            let mut stmt = self.conn.prepare(&sql)?;
            let result = stmt.query_row(params![like_pattern], |row| {
                Ok(row_reader::read_attachment(row, &self.schema))
            });

            match result {
                Ok(att) => return Ok(Some(att)),
                Err(rusqlite::Error::QueryReturnedNoRows) => continue,
                Err(e) => return Err(e.into()),
            }
        }

        Ok(None)
    }

    /// Get the iMessage account login string.
    pub fn get_imessage_account(&self) -> Result<Option<String>> {
        let result = self.conn.query_row(
            "SELECT account_login FROM chat WHERE service_name = 'iMessage' ORDER BY ROWID DESC LIMIT 1",
            [],
            |row| row.get::<_, Option<String>>(0),
        );

        match result {
            Ok(Some(login)) => Ok(login.split(':').next_back().map(|s| s.to_string())),
            Ok(None) => Ok(None),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get the last message in a chat by GUID.
    pub fn get_chat_last_message(&self, chat_guid: &str) -> Result<Option<Message>> {
        let msg_cols = self.schema.message_select_columns();
        let msg_select = msg_cols.join(", ");

        let sql = format!(
            "SELECT {msg_select}, \
             handle.ROWID AS h_ROWID, handle.id AS h_id, handle.country AS h_country, \
             handle.service AS h_service, handle.uncanonicalized_id AS h_uncanonicalized_id \
             FROM message \
             LEFT JOIN handle ON message.handle_id = handle.ROWID \
             INNER JOIN chat_message_join ON message.ROWID = chat_message_join.message_id \
             INNER JOIN chat ON chat.ROWID = chat_message_join.chat_id \
             WHERE chat.guid = ? \
             ORDER BY message.date DESC \
             LIMIT 1"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let result = stmt.query_row(params![chat_guid], |row| {
            let mut msg = row_reader::read_message(row, &self.schema);
            msg.handle = row_reader::read_handle_from_join(row, "h_");
            Ok(msg)
        });

        match result {
            Ok(msg) => Ok(Some(msg)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // =========================================================================
    // Multi-row queries
    // =========================================================================

    /// Get messages with pagination and filters. Returns (messages, total_count).
    pub fn get_messages(&self, params: &MessageQueryParams) -> Result<(Vec<Message>, i64)> {
        let msg_cols = self.schema.message_select_columns();
        let msg_select = msg_cols.join(", ");

        let mut sql = format!(
            "SELECT {msg_select}, \
             handle.ROWID AS h_ROWID, handle.id AS h_id, handle.country AS h_country, \
             handle.service AS h_service, handle.uncanonicalized_id AS h_uncanonicalized_id \
             FROM message \
             LEFT JOIN handle ON message.handle_id = handle.ROWID"
        );

        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];

        // Chat filter
        if let Some(ref guid) = params.chat_guid {
            sql.push_str(
                " INNER JOIN chat_message_join ON message.ROWID = chat_message_join.message_id \
                 INNER JOIN chat ON chat.ROWID = chat_message_join.chat_id",
            );
            sql.push_str(" WHERE chat.guid = ?");
            bind_values.push(Box::new(guid.clone()));
        } else if params.with_chats {
            sql.push_str(
                " INNER JOIN chat_message_join ON message.ROWID = chat_message_join.message_id \
                 INNER JOIN chat ON chat.ROWID = chat_message_join.chat_id",
            );
            sql.push_str(" WHERE 1=1");
        } else {
            sql.push_str(" WHERE 1=1");
        }

        // Date filters
        if params.after.is_some() || params.before.is_some() {
            self.append_date_sql(&mut sql, &mut bind_values, params.after, params.before);
        }

        // Custom WHERE clauses
        apply_where_clauses(&mut sql, &mut bind_values, &params.where_clauses);

        // Order, offset, limit
        let order_col = &params.order_by;
        let sort = params.sort.as_sql();
        sql.push_str(&format!(" ORDER BY {order_col} {sort}"));
        sql.push_str(&format!(" LIMIT {} OFFSET {}", params.limit, params.offset));

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(bind_refs.as_slice(), |row| {
            let mut msg = row_reader::read_message(row, &self.schema);
            msg.handle = row_reader::read_handle_from_join(row, "h_");
            Ok(msg)
        })?;

        let mut messages: Vec<Message> = vec![];
        for row_result in rows {
            messages.push(row_result?);
        }

        // Fetch attachments for each message if requested
        if params.with_attachments {
            for msg in &mut messages {
                msg.attachments = self.get_attachments_for_message(msg.rowid)?;
            }
        }

        // Fetch chats for each message if requested (and not already joined via chat_guid)
        if params.with_chats && params.chat_guid.is_none() {
            for msg in &mut messages {
                msg.chats = self.get_chats_for_message(msg.rowid)?;
            }
        }

        // Get total count (using a separate COUNT query for accuracy with pagination)
        let count_params = MessageCountParams {
            chat_guid: params.chat_guid.clone(),
            after: params.after,
            before: params.before,
            where_clauses: params.where_clauses.clone(),
            ..Default::default()
        };
        let total = self.get_message_count(&count_params)?;

        Ok((messages, total))
    }

    /// Get updated messages (delivered, read, edited, retracted since a date).
    pub fn get_updated_messages(&self, params: &UpdatedMessageQueryParams) -> Result<Vec<Message>> {
        let msg_cols = self.schema.message_select_columns();
        let msg_select = msg_cols.join(", ");

        let mut sql = format!(
            "SELECT {msg_select}, \
             handle.ROWID AS h_ROWID, handle.id AS h_id, handle.country AS h_country, \
             handle.service AS h_service, handle.uncanonicalized_id AS h_uncanonicalized_id \
             FROM message \
             LEFT JOIN handle ON message.handle_id = handle.ROWID"
        );

        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];

        if let Some(ref guid) = params.chat_guid {
            sql.push_str(
                " INNER JOIN chat_message_join ON message.ROWID = chat_message_join.message_id \
                 INNER JOIN chat ON chat.ROWID = chat_message_join.chat_id",
            );
            sql.push_str(" WHERE chat.guid = ?");
            bind_values.push(Box::new(guid.clone()));
        } else if params.with_chats {
            sql.push_str(
                " INNER JOIN chat_message_join ON message.ROWID = chat_message_join.message_id \
                 INNER JOIN chat ON chat.ROWID = chat_message_join.chat_id",
            );
            sql.push_str(" WHERE 1=1");
        } else {
            sql.push_str(" WHERE 1=1");
        }

        if params.after.is_some() || params.before.is_some() {
            self.append_update_date_sql(
                &mut sql,
                &mut bind_values,
                params.after,
                params.before,
                params.include_created,
            );
        }

        let sort = params.sort.as_sql();
        sql.push_str(&format!(" ORDER BY message.date {sort}"));
        sql.push_str(&format!(" LIMIT {} OFFSET {}", params.limit, params.offset));

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(bind_refs.as_slice(), |row| {
            let mut msg = row_reader::read_message(row, &self.schema);
            msg.handle = row_reader::read_handle_from_join(row, "h_");
            Ok(msg)
        })?;

        let mut messages: Vec<Message> = vec![];
        for row_result in rows {
            messages.push(row_result?);
        }

        if params.with_attachments {
            for msg in &mut messages {
                msg.attachments = self.get_attachments_for_message(msg.rowid)?;
            }
        }

        Ok(messages)
    }

    /// Get chats with pagination and filters. Returns (chats, total_count).
    pub fn get_chats(&self, params: &ChatQueryParams) -> Result<(Vec<Chat>, i64)> {
        let chat_cols = self.schema.chat_select_columns();
        let chat_select = chat_cols.join(", ");

        let mut sql = format!("SELECT {chat_select} FROM chat WHERE 1=1");
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];

        if !params.with_archived {
            sql.push_str(" AND chat.is_archived = 0");
        }

        if let Some(ref guid) = params.chat_guid {
            if params.glob_guid {
                sql.push_str(" AND chat.guid LIKE ?");
                bind_values.push(Box::new(format!("%{guid}%")));
            } else {
                sql.push_str(" AND chat.guid = ?");
                bind_values.push(Box::new(guid.clone()));
            }
        }

        let order_col = &params.order_by;
        sql.push_str(&format!(" ORDER BY {order_col} DESC"));

        if let Some(limit) = params.limit {
            sql.push_str(&format!(" LIMIT {limit} OFFSET {}", params.offset));
        }

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(bind_refs.as_slice(), |row| {
            Ok(row_reader::read_chat(row, &self.schema))
        })?;

        let mut chats: Vec<Chat> = vec![];
        for row_result in rows {
            chats.push(row_result?);
        }

        // Fetch participants for each chat
        if params.with_participants {
            for chat in &mut chats {
                chat.participants = self.get_chat_participants(chat.rowid)?;
            }
        }

        // Fetch last message for each chat
        if params.with_last_message {
            for chat in &mut chats {
                if let Some(msg) = self.get_chat_last_message(&chat.guid)? {
                    chat.messages = vec![msg];
                }
            }
        }

        // Total count (with same filters as the main query)
        let mut count_sql = String::from("SELECT COUNT(*) FROM chat WHERE 1=1");
        let mut count_binds: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];

        if !params.with_archived {
            count_sql.push_str(" AND chat.is_archived = 0");
        }
        if let Some(ref guid) = params.chat_guid {
            if params.glob_guid {
                count_sql.push_str(" AND chat.guid LIKE ?");
                count_binds.push(Box::new(format!("%{guid}%")));
            } else {
                count_sql.push_str(" AND chat.guid = ?");
                count_binds.push(Box::new(guid.clone()));
            }
        }

        let count_refs: Vec<&dyn rusqlite::types::ToSql> =
            count_binds.iter().map(|b| b.as_ref()).collect();
        let count: i64 = self
            .conn
            .query_row(&count_sql, count_refs.as_slice(), |row| row.get(0))?;

        Ok((chats, count))
    }

    /// Get handles with pagination. Returns (handles, total_count).
    pub fn get_handles(&self, params: &HandleQueryParams) -> Result<(Vec<Handle>, i64)> {
        let mut sql = String::from(
            "SELECT handle.ROWID, handle.id, handle.country, handle.service, \
             handle.uncanonicalized_id FROM handle",
        );
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];

        if let Some(ref address) = params.address {
            // Strip + prefix for matching
            let stripped = address.replace('+', "");
            sql.push_str(" WHERE handle.id LIKE ?");
            bind_values.push(Box::new(format!("%{stripped}")));
        }

        sql.push_str(&format!(" LIMIT {} OFFSET {}", params.limit, params.offset));

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(bind_refs.as_slice(), |row| Ok(row_reader::read_handle(row)))?;

        let mut handles: Vec<Handle> = vec![];
        for row_result in rows {
            handles.push(row_result?);
        }

        let count = self.get_handle_count(params.address.as_deref())?;

        Ok((handles, count))
    }

    // =========================================================================
    // Relation helpers
    // =========================================================================

    /// Get all attachments for a message by message ROWID.
    pub fn get_attachments_for_message(&self, message_rowid: i64) -> Result<Vec<Attachment>> {
        let att_cols = self.schema.attachment_select_columns();
        let att_select = att_cols.join(", ");

        let sql = format!(
            "SELECT {att_select} FROM attachment \
             INNER JOIN message_attachment_join ON attachment.ROWID = message_attachment_join.attachment_id \
             WHERE message_attachment_join.message_id = ?"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![message_rowid], |row| {
            Ok(row_reader::read_attachment(row, &self.schema))
        })?;

        let mut attachments = vec![];
        for row_result in rows {
            attachments.push(row_result?);
        }
        Ok(attachments)
    }

    /// Get all chats that a message belongs to (by message ROWID).
    fn get_chats_for_message(&self, message_rowid: i64) -> Result<Vec<Chat>> {
        let chat_cols = self.schema.chat_select_columns();
        let chat_select = chat_cols.join(", ");

        let sql = format!(
            "SELECT {chat_select} FROM chat \
             INNER JOIN chat_message_join ON chat.ROWID = chat_message_join.chat_id \
             WHERE chat_message_join.message_id = ?"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![message_rowid], |row| {
            Ok(row_reader::read_chat(row, &self.schema))
        })?;

        let mut chats = vec![];
        for row_result in rows {
            chats.push(row_result?);
        }
        Ok(chats)
    }

    /// Get all participants (handles) for a chat by chat ROWID.
    /// Preserves insertion order from the join table.
    fn get_chat_participants(&self, chat_rowid: i64) -> Result<Vec<Handle>> {
        // Get the ordered handle IDs from the join table
        let mut join_stmt = self
            .conn
            .prepare("SELECT handle_id FROM chat_handle_join WHERE chat_id = ?")?;
        let handle_ids: Vec<i64> = join_stmt
            .query_map(params![chat_rowid], |row| row.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .collect();

        if handle_ids.is_empty() {
            return Ok(vec![]);
        }

        // Fetch all handles in bulk
        let placeholders: Vec<String> = handle_ids.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT handle.ROWID, handle.id, handle.country, handle.service, \
             handle.uncanonicalized_id FROM handle WHERE handle.ROWID IN ({})",
            placeholders.join(",")
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> = handle_ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();

        let rows = stmt.query_map(params_refs.as_slice(), |row| {
            Ok(row_reader::read_handle(row))
        })?;

        let mut handle_map: HashMap<i64, Handle> = HashMap::new();
        for row_result in rows {
            let h = row_result?;
            handle_map.insert(h.rowid, h);
        }

        // Return handles in join-table order (preserving participant order)
        let handles: Vec<Handle> = handle_ids
            .iter()
            .filter_map(|id| handle_map.remove(id))
            .collect();

        Ok(handles)
    }

    // =========================================================================
    // Statistics queries
    // =========================================================================

    /// Get chats whose `last_read_message_timestamp` >= the given Apple timestamp.
    /// Used by the chat update poller to detect read status changes.
    pub fn get_chats_read_since(&self, after_apple_ts: i64) -> Result<Vec<Chat>> {
        let chat_cols = self.schema.chat_select_columns();
        let chat_select = chat_cols.join(", ");

        let sql = format!(
            "SELECT {chat_select} FROM chat \
             WHERE chat.last_read_message_timestamp >= ? \
             ORDER BY chat.last_read_message_timestamp DESC"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([after_apple_ts], |row| {
            Ok(row_reader::read_chat(row, &self.schema))
        })?;

        let mut chats = Vec::new();
        for row_result in rows {
            chats.push(row_result?);
        }
        Ok(chats)
    }

    /// Get media counts (total across all chats).
    pub fn get_media_counts(&self, media_type: &str, after: Option<i64>) -> Result<i64> {
        let mime_prefix = if media_type == "location" {
            "text/x-vlocation".to_string()
        } else {
            media_type.to_string()
        };

        let (sql, bind_values): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
            if let Some(after_ms) = after {
                let apple_ts = date_to_db(after_ms);
                (
                    "SELECT COUNT(attachment.ROWID) AS media_count \
                     FROM attachment \
                     WHERE attachment.created_date >= ? AND attachment.mime_type LIKE ?"
                        .to_string(),
                    vec![
                        Box::new(apple_ts) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(format!("{mime_prefix}%")),
                    ],
                )
            } else {
                (
                    "SELECT COUNT(attachment.ROWID) AS media_count \
                 FROM attachment \
                 WHERE attachment.mime_type LIKE ?"
                        .to_string(),
                    vec![Box::new(format!("{mime_prefix}%")) as Box<dyn rusqlite::types::ToSql>],
                )
            };

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|b| b.as_ref()).collect();
        let count: i64 = self
            .conn
            .query_row(&sql, bind_refs.as_slice(), |row| row.get(0))?;
        Ok(count)
    }

    /// Get media counts scoped to a specific chat.
    pub fn get_media_counts_by_chat(&self, chat_guid: &str, media_type: &str) -> Result<i64> {
        let mime_prefix = if media_type == "location" {
            "text/x-vlocation".to_string()
        } else {
            media_type.to_string()
        };

        let sql = "SELECT COUNT(attachment.ROWID) AS media_count \
                   FROM attachment \
                   INNER JOIN message_attachment_join ON attachment.ROWID = message_attachment_join.attachment_id \
                   INNER JOIN chat_message_join ON message_attachment_join.message_id = chat_message_join.message_id \
                   INNER JOIN chat ON chat.ROWID = chat_message_join.chat_id \
                   WHERE chat.guid = ? AND attachment.mime_type LIKE ?";

        let count: i64 = self.conn.query_row(
            sql,
            rusqlite::params![chat_guid, format!("{mime_prefix}%")],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    // =========================================================================
    // Internal SQL helpers
    // =========================================================================

    /// Append date filter (message.date) to the SQL.
    fn append_date_sql(
        &self,
        sql: &mut String,
        bind_values: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        after: Option<i64>,
        before: Option<i64>,
    ) {
        sql.push_str(" AND (1=1");
        if let Some(after_ms) = after {
            sql.push_str(" AND message.date >= ?");
            bind_values.push(Box::new(date_to_db(after_ms)));
        }
        if let Some(before_ms) = before {
            sql.push_str(" AND message.date <= ?");
            bind_values.push(Box::new(date_to_db(before_ms)));
        }
        sql.push(')');
    }

    /// Append "updated" date filter: checks date_delivered, date_read, and optionally
    /// date_edited/date_retracted (Ventura+) and date (if includeCreated).
    fn append_update_date_sql(
        &self,
        sql: &mut String,
        bind_values: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
        after: Option<i64>,
        before: Option<i64>,
        include_created: bool,
    ) {
        sql.push_str(" AND (");
        let mut first = true;

        // Helper closure to add an OR clause for a date column
        let add_date_clause = |sql: &mut String,
                               bind_values: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
                               col: &str,
                               first: &mut bool| {
            if !*first {
                sql.push_str(" OR ");
            }
            *first = false;
            sql.push_str("(1=1");
            if let Some(after_ms) = after {
                sql.push_str(&format!(" AND {col} >= ?"));
                bind_values.push(Box::new(date_to_db(after_ms)));
            }
            if let Some(before_ms) = before {
                sql.push_str(&format!(" AND {col} <= ?"));
                bind_values.push(Box::new(date_to_db(before_ms)));
            }
            sql.push(')');
        };

        if include_created {
            add_date_clause(sql, bind_values, "message.date", &mut first);
        }

        add_date_clause(sql, bind_values, "message.date_delivered", &mut first);
        add_date_clause(sql, bind_values, "message.date_read", &mut first);
        add_date_clause(sql, bind_values, "message.date_edited", &mut first);
        add_date_clause(sql, bind_values, "message.date_retracted", &mut first);

        sql.push(')');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expand_simple_named_param() {
        let mut binds: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
        let mut args = HashMap::new();
        args.insert("term".to_string(), json!("%hello%"));
        let result =
            expand_named_params("message.text LIKE :term COLLATE NOCASE", &args, &mut binds);
        assert_eq!(result, "message.text LIKE ? COLLATE NOCASE");
        assert_eq!(binds.len(), 1);
    }

    #[test]
    fn expand_spread_param() {
        let mut binds: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
        let mut args = HashMap::new();
        args.insert("guids".to_string(), json!(["g1", "g2", "g3"]));
        let result = expand_named_params("message.guid IN (:...guids)", &args, &mut binds);
        assert_eq!(result, "message.guid IN (?, ?, ?)");
        assert_eq!(binds.len(), 3);
    }

    #[test]
    fn expand_multiple_params() {
        let mut binds: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
        let mut args = HashMap::new();
        args.insert("after".to_string(), json!(100));
        args.insert("before".to_string(), json!(200));
        let result = expand_named_params(
            "message.date >= :after AND message.date <= :before",
            &args,
            &mut binds,
        );
        assert_eq!(result, "message.date >= ? AND message.date <= ?");
        assert_eq!(binds.len(), 2);
    }

    #[test]
    fn expand_no_params() {
        let mut binds: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
        let args = HashMap::new();
        let result = expand_named_params("message.is_from_me = 1", &args, &mut binds);
        assert_eq!(result, "message.is_from_me = 1");
        assert_eq!(binds.len(), 0);
    }

    #[test]
    fn apply_where_clauses_empty() {
        let mut sql = "SELECT * FROM message WHERE 1=1".to_string();
        let mut binds: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
        apply_where_clauses(&mut sql, &mut binds, &[]);
        assert_eq!(sql, "SELECT * FROM message WHERE 1=1");
    }

    #[test]
    fn apply_where_clauses_adds_and() {
        let mut sql = "SELECT * FROM message WHERE 1=1".to_string();
        let mut binds: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
        let mut args = HashMap::new();
        args.insert("val".to_string(), json!(1));
        let clauses = vec![WhereClause {
            statement: "message.is_from_me = :val".to_string(),
            args,
        }];
        apply_where_clauses(&mut sql, &mut binds, &clauses);
        assert_eq!(
            sql,
            "SELECT * FROM message WHERE 1=1 AND (message.is_from_me = ?)"
        );
        assert_eq!(binds.len(), 1);
    }
}
