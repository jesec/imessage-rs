/// Query parameter types for the iMessage database.
use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

/// A single WHERE clause item.
/// The `statement` is a SQL fragment with named parameters (`:name` or `:...name` for arrays).
/// The `args` map binds parameter names to values.
#[derive(Debug, Clone, Deserialize)]
pub struct WhereClause {
    pub statement: String,
    #[serde(default)]
    pub args: HashMap<String, Value>,
}

/// Parameters for querying messages.
#[derive(Debug, Clone)]
pub struct MessageQueryParams {
    pub chat_guid: Option<String>,
    pub offset: i64,
    pub limit: i64,
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub with_chats: bool,
    pub with_chat_participants: bool,
    pub with_attachments: bool,
    pub sort: SortOrder,
    pub order_by: String,
    pub where_clauses: Vec<WhereClause>,
}

impl Default for MessageQueryParams {
    fn default() -> Self {
        Self {
            chat_guid: None,
            offset: 0,
            limit: 100,
            after: None,
            before: None,
            with_chats: false,
            with_chat_participants: false,
            with_attachments: true,
            sort: SortOrder::Desc,
            order_by: "message.date".to_string(),
            where_clauses: vec![],
        }
    }
}

/// Parameters for querying updated messages.
#[derive(Debug, Clone)]
pub struct UpdatedMessageQueryParams {
    pub chat_guid: Option<String>,
    pub offset: i64,
    pub limit: i64,
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub with_chats: bool,
    pub with_attachments: bool,
    pub include_created: bool,
    pub sort: SortOrder,
}

impl Default for UpdatedMessageQueryParams {
    fn default() -> Self {
        Self {
            chat_guid: None,
            offset: 0,
            limit: 100,
            after: None,
            before: None,
            with_chats: false,
            with_attachments: true,
            include_created: false,
            sort: SortOrder::Desc,
        }
    }
}

/// Parameters for querying chats.
#[derive(Debug, Clone)]
pub struct ChatQueryParams {
    pub chat_guid: Option<String>,
    pub glob_guid: bool,
    pub with_participants: bool,
    pub with_last_message: bool,
    pub with_archived: bool,
    pub offset: i64,
    pub limit: Option<i64>,
    pub order_by: String,
}

impl Default for ChatQueryParams {
    fn default() -> Self {
        Self {
            chat_guid: None,
            glob_guid: false,
            with_participants: true,
            with_last_message: false,
            with_archived: true,
            offset: 0,
            limit: None,
            order_by: "chat.ROWID".to_string(),
        }
    }
}

/// Parameters for querying handles.
#[derive(Debug, Clone)]
pub struct HandleQueryParams {
    pub address: Option<String>,
    pub offset: i64,
    pub limit: i64,
}

impl Default for HandleQueryParams {
    fn default() -> Self {
        Self {
            address: None,
            offset: 0,
            limit: 1000,
        }
    }
}

/// Parameters for counting messages.
#[derive(Debug, Clone, Default)]
pub struct MessageCountParams {
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub is_from_me: bool,
    pub chat_guid: Option<String>,
    pub updated: bool,
    pub min_row_id: Option<i64>,
    pub max_row_id: Option<i64>,
    pub where_clauses: Vec<WhereClause>,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Asc,
    Desc,
}

impl SortOrder {
    pub fn as_sql(&self) -> &'static str {
        match self {
            SortOrder::Asc => "ASC",
            SortOrder::Desc => "DESC",
        }
    }
}

impl std::fmt::Display for SortOrder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_sql())
    }
}
