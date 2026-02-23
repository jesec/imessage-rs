/// Database pollers that detect new/updated messages and chat read-status changes.
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use imessage_core::dates::unix_ms_to_apple;
use imessage_core::events;
use imessage_db::imessage::entities::Message;
use imessage_db::imessage::repository::MessageRepository;
use imessage_db::imessage::types::{SortOrder, UpdatedMessageQueryParams};
use imessage_serializers::config::{AttachmentSerializerConfig, MessageSerializerConfig};
use imessage_serializers::message::serialize_message;

/// An event emitted by the pollers.
#[derive(Debug, Clone)]
pub struct WatcherEvent {
    pub event_type: String,
    pub data: Value,
}

/// Cached state for a message (to detect updates).
#[derive(Debug, Clone)]
struct MessageState {
    date_created: i64,
    is_delivered: bool,
    date_delivered: i64,
    date_read: i64,
    date_edited: i64,
    date_retracted: i64,
    did_notify_recipient: bool,
    cache_time: Instant,
}

/// Cached state for a chat (to detect read-status changes).
#[derive(Debug, Clone)]
struct ChatState {
    last_read_message_timestamp: i64,
    cache_time: Instant,
}

/// Combined poller state: tracks seen messages, message states, chat states.
pub struct PollerState {
    /// GUIDs of messages we've seen (for new vs. update detection).
    /// Values are insertion timestamps for time-bounded eviction.
    seen_guids: HashMap<String, Instant>,
    /// Cached message states for update detection.
    message_states: HashMap<String, MessageState>,
    /// Cached chat states for read-status detection.
    chat_states: HashMap<String, ChatState>,
    /// Cache TTL (1 hour).
    cache_ttl: Duration,
}

impl PollerState {
    pub fn new() -> Self {
        Self {
            seen_guids: HashMap::new(),
            message_states: HashMap::new(),
            chat_states: HashMap::new(),
            cache_ttl: Duration::from_secs(3600),
        }
    }

    /// Trim all caches, removing entries older than cache_ttl.
    pub fn trim_caches(&mut self) {
        let now = Instant::now();
        self.seen_guids
            .retain(|_, ts| now.duration_since(*ts) < self.cache_ttl);
        self.message_states
            .retain(|_, v| now.duration_since(v.cache_time) < self.cache_ttl);
        self.chat_states
            .retain(|_, v| now.duration_since(v.cache_time) < self.cache_ttl);
    }
}

impl Default for PollerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Poll for new and updated messages.
///
/// `after_unix_ms` is the Unix-ms timestamp to look back from (typically lastCheck - 30s).
/// Returns a list of events to emit.
pub fn poll_messages(
    repo: &MessageRepository,
    state: &mut PollerState,
    after_unix_ms: i64,
) -> Vec<WatcherEvent> {
    let mut events_out = Vec::new();

    // Query messages updated since `after_unix_ms`.
    // The SQL has OR clauses for date, date_delivered, date_read, date_edited, date_retracted,
    // so messages whose creation date is older but were recently delivered/read/edited are caught.
    let messages = match repo.get_updated_messages(&UpdatedMessageQueryParams {
        after: Some(after_unix_ms),
        with_chats: true,
        with_attachments: true,
        include_created: true,
        limit: 1000,
        sort: SortOrder::Asc,
        ..Default::default()
    }) {
        Ok(msgs) => {
            tracing::debug!(
                "poll_messages: query returned {} messages (after_unix_ms={})",
                msgs.len(),
                after_unix_ms
            );
            msgs
        }
        Err(e) => {
            tracing::warn!("Failed to poll messages: {e}");
            return events_out;
        }
    };

    // Process group changes first (messages with itemType 1-3 and empty text)
    let mut group_change_rowids = HashSet::new();
    for msg in &messages {
        let item_type = msg.item_type;
        if (1..=3).contains(&item_type) && is_text_empty(&msg.text) {
            let key = format!("group-change-{}", msg.rowid);
            if state.seen_guids.contains_key(&key) {
                continue;
            }
            state.seen_guids.insert(key, Instant::now());
            group_change_rowids.insert(msg.rowid);

            let group_action = msg.group_action_type;
            let event_type = match (item_type, group_action) {
                (1, 0) => events::PARTICIPANT_ADDED,
                (1, 1) => events::PARTICIPANT_REMOVED,
                (2, _) => events::GROUP_NAME_CHANGE,
                (3, 0) => events::PARTICIPANT_LEFT,
                (3, 1) => events::GROUP_ICON_CHANGED,
                (3, 2) => events::GROUP_ICON_REMOVED,
                _ => continue,
            };

            let msg_config = MessageSerializerConfig {
                load_chat_participants: true,
                include_chats: true,
                ..Default::default()
            };
            let att_config = AttachmentSerializerConfig::default();
            let data = serialize_message(msg, &msg_config, &att_config, true);

            events_out.push(WatcherEvent {
                event_type: event_type.to_string(),
                data,
            });
        }
    }

    // Now process normal messages (filter by date fields >= after_unix_ms)
    let mut relevant_count = 0u32;
    let mut new_count = 0u32;
    let mut seen_count = 0u32;
    let mut skipped_group = 0u32;
    let mut skipped_irrelevant = 0u32;
    for msg in &messages {
        if group_change_rowids.contains(&msg.rowid) {
            skipped_group += 1;
            continue;
        }

        // Check if any relevant date field is >= after_unix_ms
        if !is_message_relevant(msg, after_unix_ms) {
            skipped_irrelevant += 1;
            continue;
        }
        relevant_count += 1;

        let guid = &msg.guid;

        // Determine if this is a new or updated message
        if !state.seen_guids.contains_key(guid) {
            new_count += 1;
            // New message
            state.seen_guids.insert(guid.clone(), Instant::now());
            cache_message_state(&mut state.message_states, msg);

            let msg_config = MessageSerializerConfig {
                load_chat_participants: true,
                include_chats: true,
                ..Default::default()
            };
            let att_config = AttachmentSerializerConfig::default();
            let data = serialize_message(msg, &msg_config, &att_config, true);

            events_out.push(WatcherEvent {
                event_type: events::NEW_MESSAGE.to_string(),
                data,
            });
        } else {
            seen_count += 1;
            // Check if it's actually updated
            if let Some(prev) = state.message_states.get(guid) {
                if is_message_updated(msg, prev) {
                    cache_message_state(&mut state.message_states, msg);

                    let msg_config = MessageSerializerConfig {
                        load_chat_participants: false,
                        include_chats: false,
                        ..Default::default()
                    };
                    let att_config = AttachmentSerializerConfig::default();
                    let data = serialize_message(msg, &msg_config, &att_config, true);

                    events_out.push(WatcherEvent {
                        event_type: events::MESSAGE_UPDATED.to_string(),
                        data,
                    });
                }
            } else {
                // No cached state — cache it now
                cache_message_state(&mut state.message_states, msg);
            }
        }
    }

    tracing::debug!(
        "poll_messages: total={}, skipped_group={skipped_group}, skipped_irrelevant={skipped_irrelevant}, \
         relevant={relevant_count}, new={new_count}, seen={seen_count}, events={}",
        messages.len(),
        events_out.len()
    );

    events_out
}

/// Poll for chat read-status changes.
pub fn poll_chat_reads(
    repo: &MessageRepository,
    state: &mut PollerState,
    after_unix_ms: i64,
) -> Vec<WatcherEvent> {
    let mut events_out = Vec::new();

    let after_apple = unix_ms_to_apple(after_unix_ms);
    let chats = match repo.get_chats_read_since(after_apple) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to poll chat reads: {e}");
            return events_out;
        }
    };

    for chat in &chats {
        let ts = chat.last_read_message_timestamp.unwrap_or(0);
        let guid = &chat.guid;

        let should_emit = match state.chat_states.get(guid) {
            Some(prev) => ts > prev.last_read_message_timestamp,
            None => true,
        };

        if should_emit {
            state.chat_states.insert(
                guid.clone(),
                ChatState {
                    last_read_message_timestamp: ts,
                    cache_time: Instant::now(),
                },
            );

            events_out.push(WatcherEvent {
                event_type: events::CHAT_READ_STATUS_CHANGED.to_string(),
                data: json!({
                    "chatGuid": guid,
                    "read": true,
                }),
            });
        }
    }

    events_out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_text_empty(text: &Option<String>) -> bool {
    text.as_ref().is_none_or(|t| t.trim().is_empty())
}

/// Check if a message has any relevant date field >= the given threshold.
///
/// Note: Message date fields are already in Unix ms (converted by the row mapper),
/// so we compare directly without calling `apple_to_unix_ms`.
fn is_message_relevant(msg: &Message, after_unix_ms: i64) -> bool {
    let check_date =
        |val: Option<i64>| -> bool { val.is_some_and(|ms| ms > 0 && ms >= after_unix_ms) };

    check_date(msg.date)
        || check_date(msg.date_delivered)
        || check_date(msg.date_read)
        || check_date(msg.date_edited)
        || check_date(msg.date_retracted)
}

/// Check if a message has changed compared to its cached state.
///
/// Note: Message date fields are already in Unix ms (converted by the row mapper).
fn is_message_updated(msg: &Message, prev: &MessageState) -> bool {
    let date_created = msg.date.unwrap_or(0);
    if date_created > prev.date_created {
        return true;
    }

    let date_delivered = msg.date_delivered.unwrap_or(0);
    if date_delivered > prev.date_delivered {
        return true;
    }

    if msg.is_delivered != prev.is_delivered {
        return true;
    }

    let date_read = msg.date_read.unwrap_or(0);
    if date_read > prev.date_read {
        return true;
    }

    let date_edited = msg.date_edited.unwrap_or(0);
    if date_edited > prev.date_edited {
        return true;
    }

    let date_retracted = msg.date_retracted.unwrap_or(0);
    if date_retracted > prev.date_retracted {
        return true;
    }

    let did_notify = msg.did_notify_recipient.unwrap_or(false);
    if did_notify != prev.did_notify_recipient {
        return true;
    }

    false
}

/// Save the current state of a message to the cache.
///
/// Note: Message date fields are already in Unix ms (converted by the row mapper).
fn cache_message_state(states: &mut HashMap<String, MessageState>, msg: &Message) {
    states.insert(
        msg.guid.clone(),
        MessageState {
            date_created: msg.date.unwrap_or(0),
            is_delivered: msg.is_delivered,
            date_delivered: msg.date_delivered.unwrap_or(0),
            date_read: msg.date_read.unwrap_or(0),
            date_edited: msg.date_edited.unwrap_or(0),
            date_retracted: msg.date_retracted.unwrap_or(0),
            did_notify_recipient: msg.did_notify_recipient.unwrap_or(false),
            cache_time: Instant::now(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_poller_state_is_empty() {
        let state = PollerState::new();
        assert!(state.seen_guids.is_empty());
        assert!(state.message_states.is_empty());
        assert!(state.chat_states.is_empty());
    }

    #[test]
    fn is_text_empty_checks() {
        assert!(is_text_empty(&None));
        assert!(is_text_empty(&Some("".to_string())));
        assert!(is_text_empty(&Some("  ".to_string())));
        assert!(!is_text_empty(&Some("hello".to_string())));
    }

    #[test]
    fn group_change_mapping() {
        // itemType=1, groupActionType=0 => participant-added
        assert_eq!(
            match (1_i64, 0_i64) {
                (1, 0) => events::PARTICIPANT_ADDED,
                (1, 1) => events::PARTICIPANT_REMOVED,
                (2, _) => events::GROUP_NAME_CHANGE,
                (3, 0) => events::PARTICIPANT_LEFT,
                (3, 1) => events::GROUP_ICON_CHANGED,
                (3, 2) => events::GROUP_ICON_REMOVED,
                _ => "",
            },
            "participant-added"
        );
    }

    #[test]
    fn cache_trim_removes_old_entries() {
        let mut state = PollerState::new();
        state.cache_ttl = Duration::from_millis(0); // expire immediately

        state.chat_states.insert(
            "test".to_string(),
            ChatState {
                last_read_message_timestamp: 100,
                cache_time: Instant::now() - Duration::from_secs(1),
            },
        );

        state.trim_caches();
        assert!(state.chat_states.is_empty());
    }
}
