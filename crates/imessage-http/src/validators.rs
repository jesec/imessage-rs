//! Request validation utilities.
//!
//! Query parameter parsing and basic validation.

/// Parse the `with` query parameter (comma-separated string or array of strings).
/// All values are lowercased for case-insensitive matching.
pub fn parse_with_query(query: Option<&str>) -> Vec<String> {
    match query {
        None => vec![],
        Some(s) => s
            .split(',')
            .map(|e| e.trim().to_lowercase())
            .filter(|e| !e.is_empty())
            .collect(),
    }
}

/// Check if a `with` query list contains any of the given values.
pub fn with_has(query: &[String], values: &[&str]) -> bool {
    values.iter().any(|v| query.iter().any(|q| q == v))
}

/// Parse an optional numeric query parameter.
pub fn parse_opt_i64(value: Option<&str>) -> Option<i64> {
    value.and_then(|s| s.parse::<i64>().ok())
}

/// On Tahoe (macOS 26+), chat GUIDs in chat.db use "any;" prefix instead of
/// "iMessage;" or "SMS;". Normalize user-supplied GUIDs to match the DB.
pub fn normalize_chat_guid(guid: &str) -> String {
    let v = imessage_core::macos::macos_version();
    if v.is_min_tahoe() {
        if let Some(rest) = guid.strip_prefix("iMessage;") {
            return format!("any;{rest}");
        }
        if let Some(rest) = guid.strip_prefix("SMS;") {
            return format!("any;{rest}");
        }
    }
    guid.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_query_none() {
        assert!(parse_with_query(None).is_empty());
    }

    #[test]
    fn parse_with_query_single() {
        let result = parse_with_query(Some("chat"));
        assert_eq!(result, vec!["chat"]);
    }

    #[test]
    fn parse_with_query_multiple() {
        let result = parse_with_query(Some("chat,attachment,chat.participants"));
        assert_eq!(result, vec!["chat", "attachment", "chat.participants"]);
    }

    #[test]
    fn parse_with_query_whitespace() {
        let result = parse_with_query(Some(" chat , attachment "));
        assert_eq!(result, vec!["chat", "attachment"]);
    }

    #[test]
    fn parse_with_query_lowercased() {
        let result = parse_with_query(Some("Chat,ATTACHMENT"));
        assert_eq!(result, vec!["chat", "attachment"]);
    }

    #[test]
    fn with_has_match() {
        let query = parse_with_query(Some("chat,attachment"));
        assert!(with_has(&query, &["chat", "chats"]));
        assert!(with_has(&query, &["attachment", "attachments"]));
        assert!(!with_has(&query, &["participants"]));
    }

    #[test]
    fn normalize_chat_guid_on_tahoe() {
        // This test verifies the logic is correct; actual behavior depends on macOS version.
        let v = imessage_core::macos::macos_version();
        let result = normalize_chat_guid("iMessage;-;+15551234567");
        if v.is_min_tahoe() {
            assert_eq!(result, "any;-;+15551234567");
        } else {
            assert_eq!(result, "iMessage;-;+15551234567");
        }
    }

    #[test]
    fn normalize_chat_guid_already_any() {
        let result = normalize_chat_guid("any;-;+15551234567");
        assert_eq!(result, "any;-;+15551234567");
    }
}
