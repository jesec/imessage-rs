/// High-level action handlers: send messages and create chats via AppleScript.
///
/// Orchestrates AppleScript templates with retry logic.
use anyhow::{Result, bail};
use imessage_core::macos::MacOsVersion;
use tracing::warn;

use crate::process::{execute_applescript, safe_execute_applescript};
use crate::scripts;

/// Send a message via AppleScript (primary + fallback).
///
/// Flow:
/// 1. Try `sendMessage` script (chat GUID approach)
/// 2. On timeout, restart Messages and retry
/// 3. If still fails and it's a DM, try `sendMessageFallback` (buddy approach)
pub async fn send_message(
    chat_guid: &str,
    message: &str,
    attachment: &str,
    _is_audio_message: bool,
    v: MacOsVersion,
    format_address: &(dyn Fn(&str) -> String + Send + Sync),
) -> Result<()> {
    // For audio messages, the caller should have already converted MP3 to CAF.
    // We just need to handle the send.

    let script = scripts::send_message(chat_guid, message, attachment, v, format_address);
    let Some(script) = script else {
        bail!("Cannot send message: missing chat GUID or content");
    };

    match execute_applescript(&script).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let err_msg = e.to_string();

            // If the error is a timeout, don't retry — the AppleScript is fundamentally
            // broken (e.g., Messages.app can't process the command). Retrying would just
            // waste time and restarting Messages kills the Private API dylib connection.
            if err_msg.contains("timed out") {
                bail!("{err_msg}");
            }

            warn!("Primary send failed: {e}, restarting Messages and retrying...");

            // Restart Messages and retry
            let restart = scripts::restart_messages(3);
            let _ = safe_execute_applescript(&restart).await;

            // Retry primary
            match execute_applescript(&script).await {
                Ok(_) => Ok(()),
                Err(retry_err) => {
                    warn!("Primary send retry failed: {retry_err}");

                    // Fallback for DMs only
                    let address = scripts::get_address_from_input(chat_guid);
                    if !address.starts_with("chat") {
                        let fallback =
                            scripts::send_message_fallback(chat_guid, message, attachment);
                        match fallback {
                            Ok(Some(fb_script)) => {
                                execute_applescript(&fb_script).await?;
                                return Ok(());
                            }
                            Ok(None) => bail!("Fallback script returned None"),
                            Err(fb_err) => bail!("All send attempts failed: {fb_err}"),
                        }
                    }
                    bail!("All send attempts failed: {retry_err}");
                }
            }
        }
    }
}

/// Ensure Messages.app is running. No-op if already running.
/// Skipped by caller when Private API mode is process-dylib (dylib handles it).
pub async fn start_messages() -> Result<()> {
    let script = scripts::start_messages();
    let _ = safe_execute_applescript(&script).await;
    Ok(())
}

/// Create a new chat with participants and an optional initial message.
pub async fn create_chat(
    participants: &[String],
    service: &str,
    message: Option<&str>,
) -> Result<String> {
    // Start Messages first
    let start = scripts::start_messages();
    let _ = safe_execute_applescript(&start).await;

    let script = scripts::start_chat(participants, service, message);
    let output = execute_applescript(&script).await?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    // Action tests require a running macOS instance with Messages.app.
    // These are tested via E2E integration tests against the live system.
}
