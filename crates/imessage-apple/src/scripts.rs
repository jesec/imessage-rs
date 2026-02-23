/// AppleScript template generators.
///
/// All functions produce AppleScript source code as a String,
/// ready to pass to `process::execute_applescript()`.
use imessage_core::macos::MacOsVersion;
use imessage_core::utils::{escape_osa_exp, is_not_empty};

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build the `set targetService to ...` fragment.
/// On Sequoia+ always uses `account`.
fn build_service_script(input_service: &str) -> String {
    format!("set targetService to 1st account whose service type = {input_service}")
}

/// Build `send "message" to <target>`.
fn build_message_script(message: &str, target: &str) -> String {
    if is_not_empty(message) {
        format!("send \"{}\" to {target}", escape_osa_exp(message))
    } else {
        String::new()
    }
}

/// Build attachment-send fragment.
fn build_attachment_script(attachment: &str, variable: &str, target: &str) -> String {
    if is_not_empty(attachment) {
        format!(
            "set {variable} to \"{}\" as POSIX file\n            send theAttachment to {target}\n            delay 1",
            escape_osa_exp(attachment)
        )
    } else {
        String::new()
    }
}

/// Extract the address portion of a chat GUID (last segment after `;`).
pub fn get_address_from_input(value: &str) -> &str {
    value.rsplit(';').next().unwrap_or(value)
}

/// Extract the service portion of a chat GUID (first segment before `;`).
/// Maps `"any"` to `"iMessage"` for Tahoe compatibility.
pub fn get_service_from_input(value: &str) -> &str {
    if !value.contains(';') {
        return "iMessage";
    }
    let service = value.split(';').next().unwrap_or("iMessage");
    if service == "any" {
        "iMessage"
    } else {
        service
    }
}

// ---------------------------------------------------------------------------
// Application control
// ---------------------------------------------------------------------------

fn start_app(app_name: &str) -> String {
    format!(
        "set appName to \"{app_name}\"\n\
        if application appName is running then\n\
            return 0\n\
        else\n\
            tell application appName to reopen\n\
        end if"
    )
}

pub fn start_messages() -> String {
    start_app("Messages")
}

// ---------------------------------------------------------------------------
// Messages send / chat scripts
// ---------------------------------------------------------------------------

/// Send a message and/or attachment to a chat by GUID.
///
/// On Tahoe, replaces iMessage/SMS prefix with "any" in the GUID.
pub fn send_message(
    chat_guid: &str,
    message: &str,
    attachment: &str,
    v: MacOsVersion,
    format_address: &(dyn Fn(&str) -> String + Send + Sync),
) -> Option<String> {
    if chat_guid.is_empty() || (message.is_empty() && attachment.is_empty()) {
        return None;
    }

    let attachment_scpt = build_attachment_script(attachment, "theAttachment", "targetChat");
    let message_scpt = build_message_script(message, "targetChat");

    if !chat_guid.contains(';') {
        return None; // Invalid GUID
    }

    let mut guid = chat_guid.to_string();

    // Format the address in DM GUIDs
    if guid.contains(";-;") {
        let parts: Vec<&str> = guid.splitn(2, ";-;").collect();
        if parts.len() == 2 {
            let service = parts[0];
            let addr = parts[1];
            let formatted = format_address(addr);
            guid = format!("{service};-;{formatted}");
        }
    }

    // Tahoe: use "any" service prefix
    if v.is_min_tahoe() {
        if guid.starts_with("iMessage;") {
            guid = format!("any;{}", &guid["iMessage;".len()..]);
        } else if guid.starts_with("SMS;") {
            guid = format!("any;{}", &guid["SMS;".len()..]);
        }
    }

    Some(format!(
        "tell application \"Messages\"\n\
            set targetChat to a reference to chat id \"{guid}\"\n\
        \n\
            {attachment_scpt}\n\
            {message_scpt}\n\
        end tell"
    ))
}

/// Fallback send for DMs only: uses participant + service approach.
pub fn send_message_fallback(
    chat_guid: &str,
    message: &str,
    attachment: &str,
) -> Result<Option<String>, String> {
    if chat_guid.is_empty() || (message.is_empty() && attachment.is_empty()) {
        return Ok(None);
    }

    let attachment_scpt = build_attachment_script(attachment, "theAttachment", "targetBuddy");
    let message_scpt = build_message_script(message, "targetBuddy");

    let address = get_address_from_input(chat_guid);
    let service = get_service_from_input(chat_guid);

    if address.starts_with("chat") {
        return Err(
            "Can't use the send message (fallback) script to text a group chat!".to_string(),
        );
    }

    let service_script = build_service_script(service);

    Ok(Some(format!(
        "tell application \"Messages\"\n\
            {service_script}\n\
            set targetBuddy to participant \"{address}\" of targetService\n\
        \n\
            {attachment_scpt}\n\
            {message_scpt}\n\
        end tell"
    )))
}

/// Restart Messages app with a configurable delay.
pub fn restart_messages(delay_seconds: u32) -> String {
    format!(
        "tell application \"Messages\"\n\
            quit\n\
            delay {delay_seconds}\n\
            reopen\n\
        end tell"
    )
}

/// Create a new chat with given participants and optional initial message.
pub fn start_chat(participants: &[String], service: &str, message: Option<&str>) -> String {
    let service_script = build_service_script(service);
    let buddies = participants
        .iter()
        .map(|b| format!("buddy \"{b}\" of targetService"))
        .collect::<Vec<_>>()
        .join(", ");

    let message_scpt = match message {
        Some(msg) if is_not_empty(msg) => build_message_script(msg, "thisChat"),
        _ => String::new(),
    };

    format!(
        "tell application \"Messages\"\n\
            {service_script}\n\
        \n\
            (* Start the new chat with all the recipients *)\n\
            set thisChat to make new chat with properties {{participants: {{{buddies}}}}}\n\
            log thisChat\n\
            {message_scpt}\n\
        end tell\n\
        \n\
        try\n\
            tell application \"System Events\" to tell process \"Messages\" to set visible to false\n\
        end try"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tahoe() -> MacOsVersion {
        MacOsVersion::new(26, 3, 0)
    }
    fn sequoia() -> MacOsVersion {
        MacOsVersion::new(15, 0, 0)
    }

    fn identity(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn get_address_extracts_last_segment() {
        assert_eq!(
            get_address_from_input("iMessage;-;+15551234567"),
            "+15551234567"
        );
        assert_eq!(get_address_from_input("SMS;-;+15551234567"), "+15551234567");
        assert_eq!(get_address_from_input("no-semicolons"), "no-semicolons");
    }

    #[test]
    fn get_service_extracts_first_segment() {
        assert_eq!(
            get_service_from_input("iMessage;-;+15551234567"),
            "iMessage"
        );
        assert_eq!(get_service_from_input("SMS;-;+15551234567"), "SMS");
        assert_eq!(get_service_from_input("any;-;+15551234567"), "iMessage"); // Tahoe maps to iMessage
        assert_eq!(get_service_from_input("no-semicolons"), "iMessage"); // default
    }

    #[test]
    fn send_message_returns_none_for_empty() {
        assert!(send_message("", "hello", "", tahoe(), &identity).is_none());
        assert!(send_message("iMessage;-;+15551234567", "", "", tahoe(), &identity).is_none());
    }

    #[test]
    fn send_message_tahoe_uses_any_prefix() {
        let script =
            send_message("iMessage;-;+15551234567", "hello", "", tahoe(), &identity).unwrap();
        assert!(script.contains("any;-;+15551234567"));
        assert!(!script.contains("iMessage;-;"));
    }

    #[test]
    fn send_message_sequoia_keeps_imessage_prefix() {
        let script =
            send_message("iMessage;-;+15551234567", "hello", "", sequoia(), &identity).unwrap();
        assert!(script.contains("iMessage;-;+15551234567"));
    }

    #[test]
    fn build_service_uses_account() {
        let s = build_service_script("iMessage");
        assert!(s.contains("1st account whose service type"));
    }

    #[test]
    fn restart_messages_includes_delay() {
        let s = restart_messages(5);
        assert!(s.contains("delay 5"));
    }

    #[test]
    fn start_chat_uses_make_new_chat() {
        let s = start_chat(&["buddy1".to_string()], "iMessage", None);
        assert!(s.contains("make new chat"));
        assert!(!s.contains("text chat"));
    }

    #[test]
    fn fallback_rejects_group_chats() {
        let result = send_message_fallback("iMessage;+;chat123456", "hello", "");
        assert!(result.is_err());
    }

    #[test]
    fn fallback_uses_participant() {
        let script = send_message_fallback("iMessage;-;+15551234567", "hello", "")
            .unwrap()
            .unwrap();
        assert!(script.contains("participant"));
    }
}
