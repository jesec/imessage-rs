/// FaceTime session management for the admit-and-leave flow.
///
/// When a FaceTime link is generated, the server briefly joins the call to act
/// as "host", admits remote users from the waiting room, then silently leaves.
/// This is necessary because FaceTime links require someone to admit participants
/// from the waiting room.
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::time::sleep;
use tracing::{debug, info, warn};

use imessage_db::notification_center;
use imessage_private_api::actions;
use imessage_private_api::service::PrivateApiService;

/// How often to poll the Notification Center DB for join requests.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How long to wait for a join request before giving up and leaving.
const ADMIT_TIMEOUT: Duration = Duration::from_secs(120);

/// How long to wait after admitting someone for them to fully connect.
const POST_ADMIT_WAIT: Duration = Duration::from_secs(15);

/// How long to wait after leaving a call for cleanup.
const POST_LEAVE_WAIT: Duration = Duration::from_secs(2);

/// How long to wait after answering before generating a link (prevents crashes on Sonoma+).
const POST_ANSWER_WAIT: Duration = Duration::from_secs(4);

/// Lookback window for notification center queries (seconds).
const NC_LOOKBACK: f64 = 5.0;

/// Generate a FaceTime link and fire-and-forget the admit-and-leave background task.
///
/// Returns the generated FaceTime link URL.
pub async fn create_session(api: &Arc<PrivateApiService>) -> Result<String, String> {
    info!("Generating FaceTime link for new call");

    // Generate link (null callUUID = new outgoing call)
    let action = actions::generate_facetime_link(None);
    let result = api.send_action(action).await.map_err(|e| e.to_string())?;

    let url = result
        .and_then(|txn| txn.data)
        .and_then(|d| d.get("url").or(d.get("link")).cloned())
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .ok_or_else(|| "Failed to generate FaceTime link!".to_string())?;

    validate_facetime_link(&url)?;

    info!("FaceTime link generated: {url}");

    // Fire-and-forget: admit-and-leave in the background
    let api_clone = api.clone();
    tokio::spawn(async move {
        if let Err(e) = admit_and_leave(&api_clone).await {
            warn!("FaceTime admit-and-leave failed: {e}");
        }
    });

    Ok(url)
}

/// Answer an incoming FaceTime call, generate a shareable link, and admit-and-leave.
///
/// Returns the generated FaceTime link URL.
pub async fn answer_call(api: &Arc<PrivateApiService>, call_uuid: &str) -> Result<String, String> {
    info!("Answering FaceTime call: {call_uuid}");

    // Step 1: Answer the call (awaits the transaction response from the dylib,
    // which implicitly waits for the call to be answered — up to 120s timeout)
    let action = actions::answer_call(call_uuid);
    let result = api.send_action(action).await.map_err(|e| e.to_string())?;
    if let Some(ref txn) = result
        && let Some(ref data) = txn.data
        && let Some(err) = data.get("error").and_then(|e| e.as_str())
    {
        return Err(format!("Failed to answer FaceTime call: {err}"));
    }

    // Additional safety margin after the call is answered (prevents crashes on Sonoma+)
    sleep(POST_ANSWER_WAIT).await;

    // Step 2: Generate a shareable link for the now-active call
    let action = actions::generate_facetime_link(Some(call_uuid));
    let result = api.send_action(action).await.map_err(|e| e.to_string())?;

    let url = result
        .and_then(|txn| txn.data)
        .and_then(|d| d.get("url").or(d.get("link")).cloned())
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .ok_or_else(|| "Failed to generate FaceTime link after answering!".to_string())?;

    validate_facetime_link(&url)?;

    info!("FaceTime link generated for answered call: {url}");

    // Fire-and-forget: admit-and-leave in the background
    let api_clone = api.clone();
    tokio::spawn(async move {
        if let Err(e) = admit_and_leave(&api_clone).await {
            warn!("FaceTime admit-and-leave failed: {e}");
        }
    });

    Ok(url)
}

/// Poll for join notifications, admit waiting-room participants, then leave the call.
///
/// This runs as a background task after generating a FaceTime link.
async fn admit_and_leave(api: &Arc<PrivateApiService>) -> Result<(), String> {
    debug!("Waiting to admit participant...");

    let admitted = admit_self(api).await?;
    if !admitted {
        // Leave the call even on timeout
        let _ = api.send_action(actions::leave_call("")).await;
        sleep(POST_LEAVE_WAIT).await;
        return Err("No join requests detected within timeout".to_string());
    }

    debug!("Waiting {POST_ADMIT_WAIT:?} for participant to fully connect...");
    sleep(POST_ADMIT_WAIT).await;

    debug!("Leaving the call...");
    // Empty string UUID tells the dylib to leave whatever active call it's in.
    let _ = api.send_action(actions::leave_call("")).await;

    sleep(POST_LEAVE_WAIT).await;
    info!("FaceTime admit-and-leave complete");

    Ok(())
}

/// Validate that a FaceTime link looks correct (non-empty, starts with https://facetime.apple.com/).
fn validate_facetime_link(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("Generated FaceTime link is empty!".to_string());
    }
    if !url.starts_with("https://facetime.apple.com/") {
        return Err(format!(
            "Generated link doesn't look like a FaceTime URL: {url}"
        ));
    }
    Ok(())
}

/// Poll the Notification Center DB for FaceTime join requests and admit them.
///
/// Returns true if someone was successfully admitted.
async fn admit_self(api: &Arc<PrivateApiService>) -> Result<bool, String> {
    let start = Instant::now();
    let mut admitted_users: Vec<String> = Vec::new();

    while start.elapsed() < ADMIT_TIMEOUT {
        match notification_center::get_facetime_join_notifications(NC_LOOKBACK) {
            Ok(notifications) => {
                debug!("Found {} FaceTime notification(s)", notifications.len());

                for notif in &notifications {
                    if admitted_users.contains(&notif.user_id) {
                        debug!("User already admitted: {}", notif.user_id);
                        continue;
                    }

                    debug!(
                        "Admitting user {} into conversation {}",
                        notif.user_id, notif.conversation_id
                    );

                    let action = actions::admit_participant(&notif.conversation_id, &notif.user_id);

                    match api.send_action(action).await {
                        Ok(_) => {
                            admitted_users.push(notif.user_id.clone());
                            info!("Admitted user {} into FaceTime call", notif.user_id);
                            return Ok(true);
                        }
                        Err(e) => {
                            warn!("Failed to admit user: {e}");
                        }
                    }
                }
            }
            Err(e) => {
                debug!("Failed to query notification center: {e}");
            }
        }

        sleep(POLL_INTERVAL).await;
    }

    warn!(
        "FaceTime admit timeout: no join requests detected in {:?}",
        ADMIT_TIMEOUT
    );
    Ok(false)
}
