//! Serializer configuration types.

/// Configuration for message serialization.
#[derive(Debug, Clone)]
pub struct MessageSerializerConfig {
    pub parse_attributed_body: bool,
    pub parse_message_summary: bool,
    pub parse_payload_data: bool,
    pub include_chats: bool,
    pub load_chat_participants: bool,
}

impl Default for MessageSerializerConfig {
    fn default() -> Self {
        Self {
            parse_attributed_body: false,
            parse_message_summary: false,
            parse_payload_data: false,
            include_chats: true,
            load_chat_participants: true,
        }
    }
}

impl MessageSerializerConfig {
    /// Config for serializing sent messages (after send/react/edit/unsend).
    /// Defaults: parseAttributedBody: true, parseMessageSummary: true,
    /// parsePayloadData: true, loadChatParticipants: false for sent message responses.
    pub fn for_sent_message() -> Self {
        Self {
            parse_attributed_body: true,
            parse_message_summary: true,
            parse_payload_data: true,
            include_chats: true,
            load_chat_participants: false,
        }
    }
}

/// Configuration for attachment serialization.
#[derive(Debug, Clone)]
pub struct AttachmentSerializerConfig {
    pub load_data: bool,
    pub load_metadata: bool,
}

impl Default for AttachmentSerializerConfig {
    fn default() -> Self {
        Self {
            load_data: false,
            load_metadata: true,
        }
    }
}

/// Configuration for chat serialization.
#[derive(Debug, Clone)]
pub struct ChatSerializerConfig {
    pub include_participants: bool,
    pub include_messages: bool,
}

impl Default for ChatSerializerConfig {
    fn default() -> Self {
        Self {
            include_participants: true,
            include_messages: false,
        }
    }
}
