//! Webhook event type constants.
//! These strings are the exact values sent in webhook payloads.

pub const NEW_MESSAGE: &str = "new-message";
pub const MESSAGE_UPDATED: &str = "updated-message";
pub const NEW_SERVER: &str = "new-server";
pub const PARTICIPANT_REMOVED: &str = "participant-removed";
pub const PARTICIPANT_ADDED: &str = "participant-added";
pub const PARTICIPANT_LEFT: &str = "participant-left";
pub const GROUP_ICON_CHANGED: &str = "group-icon-changed";
pub const GROUP_ICON_REMOVED: &str = "group-icon-removed";
pub const CHAT_READ_STATUS_CHANGED: &str = "chat-read-status-changed";
pub const HELLO_WORLD: &str = "hello-world";
pub const TYPING_INDICATOR: &str = "typing-indicator";
pub const GROUP_NAME_CHANGE: &str = "group-name-change";
pub const INCOMING_FACETIME: &str = "incoming-facetime";
pub const IMESSAGE_ALIASES_REMOVED: &str = "imessage-aliases-removed";
pub const FACETIME_CALL_STATUS_CHANGED: &str = "facetime-call-status-changed";
pub const NEW_FINDMY_LOCATION: &str = "new-findmy-location";
pub const MESSAGE_SEND_ERROR: &str = "message-send-error";
pub const SERVER_UPDATE: &str = "server-update";
/// All event types for wildcard webhook subscriptions.
pub const ALL_EVENTS: &[&str] = &[
    NEW_MESSAGE,
    MESSAGE_UPDATED,
    NEW_SERVER,
    PARTICIPANT_REMOVED,
    PARTICIPANT_ADDED,
    PARTICIPANT_LEFT,
    GROUP_ICON_CHANGED,
    GROUP_ICON_REMOVED,
    CHAT_READ_STATUS_CHANGED,
    HELLO_WORLD,
    TYPING_INDICATOR,
    GROUP_NAME_CHANGE,
    INCOMING_FACETIME,
    IMESSAGE_ALIASES_REMOVED,
    FACETIME_CALL_STATUS_CHANGED,
    NEW_FINDMY_LOCATION,
    MESSAGE_SEND_ERROR,
    SERVER_UPDATE,
];
