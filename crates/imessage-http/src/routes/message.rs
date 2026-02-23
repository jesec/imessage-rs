use axum::Json;
/// Message routes:
///   GET  /api/v1/message/count
///   GET  /api/v1/message/count/updated
///   GET  /api/v1/message/count/me
///   POST /api/v1/message/query
///   GET  /api/v1/message/:guid
///   POST /api/v1/message/text
///   POST /api/v1/message/attachment
///   POST /api/v1/message/attachment/chunk
///   POST /api/v1/message/multipart
///   POST /api/v1/message/react
///   POST /api/v1/message/:guid/edit
///   POST /api/v1/message/:guid/unsend
///   POST /api/v1/message/:guid/notify
///   GET  /api/v1/message/:guid/embedded-media
use axum::extract::{Multipart, Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use std::collections::HashMap;
use std::time::Duration;

use imessage_core::config::AppPaths;
use imessage_core::macos::macos_version;
use imessage_db::imessage::types::{
    MessageCountParams, MessageQueryParams, SortOrder, WhereClause,
};
use imessage_private_api::actions::{self, Action};
use imessage_private_api::service::PrivateApiService;
use imessage_serializers::config::{AttachmentSerializerConfig, MessageSerializerConfig};
use imessage_serializers::message::serialize_message;

use crate::awaiter;
use crate::extractors::AppJson;
use crate::middleware::error::{
    AppError, success_response, success_response_with_message, success_with_message_and_metadata,
};
use crate::path_safety::{
    resolve_relative_path_in_base, sanitize_filename, sanitize_header_filename,
    sanitize_path_component,
};
use crate::state::AppState;
use crate::validators::{normalize_chat_guid, parse_opt_i64, with_has};

// ---------------------------------------------------------------------------
// Helpers (deduplicate send-route patterns)
// ---------------------------------------------------------------------------

/// Resolve which send method to use.
/// Respects explicit method when provided; defaults to private-api when enabled.
/// Rejects unrecognized method values (must be `apple-script` or `private-api`).
fn resolve_send_method(
    explicit: Option<&str>,
    private_api_enabled: bool,
) -> Result<&'static str, AppError> {
    match explicit {
        Some("private-api") => Ok("private-api"),
        Some("apple-script") => Ok("apple-script"),
        Some(other) => Err(AppError::bad_request(&format!(
            "Invalid method '{other}'. Must be 'apple-script' or 'private-api'."
        ))),
        None if private_api_enabled => Ok("private-api"),
        None => Ok("apple-script"),
    }
}

/// Check the send cache for a temp_guid. Returns Err if already cached; otherwise caches it.
fn check_send_cache(state: &AppState, temp_guid: Option<&str>) -> Result<(), AppError> {
    if let Some(tg) = temp_guid
        && !tg.is_empty()
    {
        if state.is_send_cached(tg) {
            return Err(AppError::bad_request(
                "This message is already queued to be sent!",
            ));
        }
        state.cache_send(tg.to_string());
    }
    Ok(())
}

/// Inject tempGuid into a serialized message JSON object.
fn inject_temp_guid(data: &mut Value, temp_guid: Option<&str>) {
    if let Some(tg) = temp_guid
        && let Some(obj) = data.as_object_mut()
    {
        obj.insert("tempGuid".to_string(), json!(tg));
    }
}

/// RAII guard that uncaches a temp_guid when dropped (on success or error).
struct SendCacheGuard<'a> {
    state: &'a AppState,
    temp_guid: Option<String>,
}

impl<'a> SendCacheGuard<'a> {
    fn new(state: &'a AppState, temp_guid: Option<&str>) -> Self {
        Self {
            state,
            temp_guid: temp_guid.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        }
    }
}

impl Drop for SendCacheGuard<'_> {
    fn drop(&mut self) {
        if let Some(ref tg) = self.temp_guid {
            self.state.uncache_send(tg);
        }
    }
}

/// Send an action via Private API, await the message in the DB, and serialize it.
async fn send_and_await_private_api(
    state: &AppState,
    api: &PrivateApiService,
    action: Action,
    with_attachments: bool,
    entity_name: &str,
) -> Result<Value, AppError> {
    let result = api
        .send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    let txn = result
        .ok_or_else(|| AppError::imessage_error(&format!("Failed to send {entity_name}!")))?;
    if txn.identifier.is_empty() {
        return Err(AppError::imessage_error(&format!(
            "Failed to send {entity_name}!"
        )));
    }

    let identifier = txn.identifier.clone();
    let repo = state.imessage_repo.clone();
    let msg = awaiter::await_message(Duration::from_secs(60), || {
        let repo = repo.clone();
        let id = identifier.clone();
        async move {
            repo.lock()
                .get_message(&id, true, with_attachments)
                .ok()
                .flatten()
        }
    })
    .await
    .ok_or_else(|| {
        AppError::imessage_error(&format!(
            "Failed to send {entity_name}! Message not found in database after 60 seconds!"
        ))
    })?;

    let msg_config = MessageSerializerConfig::for_sent_message();
    let att_config = AttachmentSerializerConfig::default();
    Ok(serialize_message(&msg, &msg_config, &att_config, false))
}

/// Query params shared by count/count_updated/count_me
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CountParams {
    pub after: Option<String>,
    pub before: Option<String>,
    pub chat_guid: Option<String>,
    pub min_row_id: Option<String>,
    pub max_row_id: Option<String>,
}

/// GET /api/v1/message/count
pub async fn count(
    State(state): State<AppState>,
    Query(params): Query<CountParams>,
) -> Result<Json<Value>, AppError> {
    let total = state
        .imessage_repo
        .lock()
        .get_message_count(&MessageCountParams {
            after: parse_opt_i64(params.after.as_deref()),
            before: parse_opt_i64(params.before.as_deref()),
            chat_guid: params.chat_guid,
            min_row_id: parse_opt_i64(params.min_row_id.as_deref()),
            max_row_id: parse_opt_i64(params.max_row_id.as_deref()),
            is_from_me: false,
            updated: false,
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    Ok(Json(success_response(json!({ "total": total }))))
}

/// GET /api/v1/message/count/updated
/// The `after` parameter is required for this endpoint.
pub async fn count_updated(
    State(state): State<AppState>,
    Query(params): Query<CountParams>,
) -> Result<Json<Value>, AppError> {
    // `after` is required for count/updated
    if params.after.is_none() {
        return Err(AppError::bad_request("The after field is required."));
    }

    let total = state
        .imessage_repo
        .lock()
        .get_message_count(&MessageCountParams {
            after: parse_opt_i64(params.after.as_deref()),
            before: parse_opt_i64(params.before.as_deref()),
            chat_guid: params.chat_guid,
            min_row_id: parse_opt_i64(params.min_row_id.as_deref()),
            max_row_id: parse_opt_i64(params.max_row_id.as_deref()),
            is_from_me: false,
            updated: true,
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    Ok(Json(success_response(json!({ "total": total }))))
}

/// GET /api/v1/message/count/me
pub async fn sent_count(
    State(state): State<AppState>,
    Query(params): Query<CountParams>,
) -> Result<Json<Value>, AppError> {
    let total = state
        .imessage_repo
        .lock()
        .get_message_count(&MessageCountParams {
            after: parse_opt_i64(params.after.as_deref()),
            before: parse_opt_i64(params.before.as_deref()),
            chat_guid: params.chat_guid,
            min_row_id: parse_opt_i64(params.min_row_id.as_deref()),
            max_row_id: parse_opt_i64(params.max_row_id.as_deref()),
            is_from_me: true,
            updated: false,
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    Ok(Json(success_response(json!({ "total": total }))))
}

/// POST /api/v1/message/query body
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MessageQueryBody {
    pub chat_guid: Option<String>,
    #[serde(rename = "with")]
    pub with_query: Option<Vec<String>>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
    pub sort: Option<String>,
    pub after: Option<i64>,
    pub before: Option<i64>,
    /// Custom WHERE clauses.
    #[serde(rename = "where", default)]
    pub where_clauses: Vec<WhereClause>,
}

/// POST /api/v1/message/query
pub async fn query(
    State(state): State<AppState>,
    AppJson(body): AppJson<MessageQueryBody>,
) -> Result<Json<Value>, AppError> {
    let with_query: Vec<String> = body
        .with_query
        .unwrap_or_default()
        .iter()
        .map(|s| s.trim().to_lowercase())
        .collect();

    let with_chats = with_has(&with_query, &["chat", "chats"]);
    let with_attachments = with_has(&with_query, &["attachment", "attachments"]);
    let with_chat_participants =
        with_has(&with_query, &["chat.participants", "chats.participants"]);
    let with_attributed_body = with_has(&with_query, &["attributedbody", "attributed-body"]);
    let with_message_summary =
        with_has(&with_query, &["messagesummaryinfo", "message-summary-info"]);
    let with_payload_data = with_has(&with_query, &["payloaddata", "payload-data"]);

    let offset = body.offset.unwrap_or(0);
    let limit = body.limit.unwrap_or(100).min(1000); // max:1000

    let sort_order = match body.sort.as_deref() {
        Some("ASC") => SortOrder::Asc,
        _ => SortOrder::Desc,
    };

    // Normalize chat GUID for Tahoe (iMessage;/SMS; -> any;)
    let normalized_chat_guid = body.chat_guid.as_deref().map(normalize_chat_guid);

    // If chatGuid is specified, verify it exists
    if let Some(ref chat_guid) = normalized_chat_guid {
        let (chats, _) = state
            .imessage_repo
            .lock()
            .get_chats(&imessage_db::imessage::types::ChatQueryParams {
                chat_guid: Some(chat_guid.clone()),
                ..Default::default()
            })
            .map_err(|e| AppError::server_error(&e.to_string()))?;

        if chats.is_empty() {
            return Ok(Json(
                crate::middleware::error::success_response_with_message(
                    &format!("No chat found with GUID: {chat_guid}"),
                    json!([]),
                ),
            ));
        }
    }

    // Spotlight search: on macOS 15+, outgoing message text is NULL in the DB.
    // When a where clause targets message.text, use Spotlight via Private API instead.
    let mut where_clauses = body.where_clauses;
    if state.config.enable_private_api && !where_clauses.is_empty() {
        let text_idx = where_clauses
            .iter()
            .position(|c| c.statement.contains("message.text"));

        if let Some(idx) = text_idx {
            let clause = &where_clauses[idx];
            let parts: Vec<&str> = clause.statement.split_whitespace().collect();
            let search_term = parts
                .get(2)
                .map(|p| p.trim_start_matches(':'))
                .and_then(|var| clause.args.get(var))
                .and_then(|v| v.as_str())
                .map(|s| s.trim_matches('%').to_string())
                .filter(|s| !s.is_empty());

            if let Some(term) = search_term
                && let Ok(api) = state.require_private_api()
            {
                let action = actions::search_messages(&term, "contains");
                if let Ok(Some(txn)) = api.send_action(action).await
                    && let Some(data) = &txn.data
                    && let Some(guids) = data.get("results").and_then(|r| r.as_array())
                {
                    if guids.is_empty() {
                        return Ok(Json(success_with_message_and_metadata(
                            "Successfully fetched messages!",
                            json!([]),
                            json!({ "offset": offset, "limit": limit, "total": 0, "count": 0 }),
                        )));
                    }
                    // Replace text clause with GUID IN clause
                    where_clauses.remove(idx);
                    let mut args = HashMap::new();
                    args.insert("guids".to_string(), json!(guids));
                    where_clauses.push(WhereClause {
                        statement: "message.guid IN (:...guids)".to_string(),
                        args,
                    });
                }
            }
        }
    }

    let (messages, total_count) = state
        .imessage_repo
        .lock()
        .get_messages(&MessageQueryParams {
            chat_guid: normalized_chat_guid.clone(),
            with_chats: with_chats || with_chat_participants,
            with_attachments,
            offset,
            limit,
            sort: sort_order,
            before: body.before,
            after: body.after,
            where_clauses,
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let msg_config = MessageSerializerConfig {
        parse_attributed_body: with_attributed_body,
        parse_message_summary: with_message_summary,
        parse_payload_data: with_payload_data,
        load_chat_participants: with_chat_participants,
        ..Default::default() // include_chats defaults to true (always emit "chats" key)
    };

    let att_config = AttachmentSerializerConfig::default();

    let mut data: Vec<Value> = messages
        .iter()
        .map(|m| serialize_message(m, &msg_config, &att_config, false))
        .collect();

    // Inject chat participants if requested
    if with_chat_participants {
        use imessage_serializers::handle::serialize_handles;

        let mut participant_cache: HashMap<String, Value> = HashMap::new();
        for msg_json in &mut data {
            if let Some(chats) = msg_json.get_mut("chats").and_then(|c| c.as_array_mut()) {
                for chat_json in chats {
                    if let Some(guid) = chat_json
                        .get("guid")
                        .and_then(|g| g.as_str())
                        .map(|s| s.to_string())
                    {
                        let participants =
                            participant_cache.entry(guid.clone()).or_insert_with(|| {
                                let (chats_with_p, _) = state
                                    .imessage_repo
                                    .lock()
                                    .get_chats(&imessage_db::imessage::types::ChatQueryParams {
                                        chat_guid: Some(guid),
                                        with_participants: true,
                                        ..Default::default()
                                    })
                                    .unwrap_or_default();
                                if let Some(chat) = chats_with_p.first() {
                                    json!(serialize_handles(&chat.participants, false))
                                } else {
                                    json!([])
                                }
                            });
                        chat_json["participants"] = participants.clone();
                    }
                }
            }
        }
    }

    let metadata = json!({
        "offset": offset,
        "limit": limit,
        "total": total_count,
        "count": data.len(),
    });

    Ok(Json(success_with_message_and_metadata(
        "Successfully fetched messages!",
        json!(data),
        metadata,
    )))
}

/// GET /api/v1/message/:guid query params
#[derive(Debug, Deserialize, Default)]
pub struct MessageFindParams {
    #[serde(rename = "with")]
    pub with_query: Option<String>,
}

/// GET /api/v1/message/:guid
pub async fn find(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    Query(params): Query<MessageFindParams>,
) -> Result<Json<Value>, AppError> {
    let with_query = crate::validators::parse_with_query(params.with_query.as_deref());
    let with_chats = with_has(&with_query, &["chats", "chat"]);
    let with_chat_participants =
        with_has(&with_query, &["chat.participants", "chats.participants"]);
    let with_attachments = with_has(&with_query, &["attachment", "attachments"]);
    let with_attributed_body = with_has(&with_query, &["attributedbody", "attributed-body"]);
    let with_message_summary =
        with_has(&with_query, &["messagesummaryinfo", "message-summary-info"]);
    let with_payload_data = with_has(&with_query, &["payloaddata", "payload-data"]);

    let mut message = state
        .imessage_repo
        .lock()
        .get_message(
            &guid,
            with_chats || with_chat_participants,
            with_attachments,
        )
        .map_err(|e| AppError::server_error(&e.to_string()))?
        .ok_or_else(|| AppError::not_found("Message does not exist!"))?;

    // If chat.participants requested, fetch participants for each chat
    if with_chat_participants {
        let repo = state.imessage_repo.lock();
        for chat in &mut message.chats {
            let (chats_with_participants, _) = repo
                .get_chats(&imessage_db::imessage::types::ChatQueryParams {
                    chat_guid: Some(chat.guid.clone()),
                    with_participants: true,
                    with_archived: true,
                    ..Default::default()
                })
                .map_err(|e| AppError::server_error(&e.to_string()))?;
            if let Some(c) = chats_with_participants.first() {
                chat.participants = c.participants.clone();
            }
        }
    }

    let config = MessageSerializerConfig {
        parse_attributed_body: with_attributed_body,
        parse_message_summary: with_message_summary,
        parse_payload_data: with_payload_data,
        load_chat_participants: with_chat_participants,
        ..Default::default()
    };

    let att_config = AttachmentSerializerConfig::default();
    let data = serialize_message(&message, &config, &att_config, false);

    Ok(Json(success_response(data)))
}

// ---------------------------------------------------------------------------
// Write routes
// ---------------------------------------------------------------------------

/// POST /api/v1/message/text body
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendTextBody {
    pub chat_guid: String,
    pub temp_guid: Option<String>,
    pub message: Option<String>,
    pub method: Option<String>,
    pub effect_id: Option<String>,
    pub subject: Option<String>,
    pub selected_message_guid: Option<String>,
    pub part_index: Option<u32>,
    pub dd_scan: Option<bool>,
    pub attributed_body: Option<Value>,
    pub text_formatting: Option<Value>,
}

/// POST /api/v1/message/text
pub async fn send_text(
    State(state): State<AppState>,
    AppJson(body): AppJson<SendTextBody>,
) -> Result<Json<Value>, AppError> {
    if body.chat_guid.is_empty() {
        return Err(AppError::bad_request("chatGuid is required"));
    }

    let mut message = body.message.as_deref().unwrap_or("").to_string();
    if message.is_empty() && body.subject.is_none() && body.attributed_body.is_none() {
        return Err(AppError::bad_request(
            "message, subject, or attributedBody is required",
        ));
    }

    check_send_cache(&state, body.temp_guid.as_deref())?;
    let _cache_guard = SendCacheGuard::new(&state, body.temp_guid.as_deref());

    // Auto-stop typing indicator if one was active for this chat
    if state.typing_cache.lock().remove(&body.chat_guid)
        && let Ok(api) = state.require_private_api()
    {
        let _ = api.send_action(actions::stop_typing(&body.chat_guid)).await;
    }

    // Reject conflicting styling params
    if body.text_formatting.is_some() && body.attributed_body.is_some() {
        return Err(AppError::bad_request(
            "Cannot provide both textFormatting and attributedBody",
        ));
    }

    // Auto-convert markdown → native iMessage formatting when:
    // - Private API is enabled, markdown_to_formatting config is true
    // - message is non-empty, no explicit textFormatting or attributedBody provided
    let mut text_formatting = body.text_formatting.clone();
    if state.config.enable_private_api
        && state.config.markdown_to_formatting
        && !message.is_empty()
        && !imessage_core::formatting::has_text_formatting(text_formatting.as_ref())
        && body.attributed_body.is_none()
        && let Some(parsed) = imessage_core::formatting::parse_markdown_formatting(&message)
    {
        if !parsed.formatting.is_empty() {
            text_formatting = Some(parsed.formatting_json());
        }
        message = parsed.clean_text;
    }

    // Force private-api when features require it, even if the client specified apple-script.
    let needs_private_api = body.effect_id.is_some()
        || body.subject.is_some()
        || body.selected_message_guid.is_some()
        || body.attributed_body.is_some()
        || text_formatting.is_some()
        || body.dd_scan.is_some();

    let method = if needs_private_api {
        "private-api"
    } else {
        resolve_send_method(body.method.as_deref(), state.config.enable_private_api)?
    };

    if method == "private-api" {
        let api = state.require_private_api()?;
        let opts = actions::SendOptions {
            subject: body.subject.as_deref(),
            effect_id: body.effect_id.as_deref(),
            selected_message_guid: body.selected_message_guid.as_deref(),
            part_index: body.part_index.map(|p| p as i64),
            attributed_body: body.attributed_body.as_ref(),
        };
        let action = actions::send_message(
            &body.chat_guid,
            &message,
            &opts,
            text_formatting.as_ref(),
            body.dd_scan,
        );
        let mut data = send_and_await_private_api(&state, &api, action, false, "message").await?;
        inject_temp_guid(&mut data, body.temp_guid.as_deref());

        let msg_error = data.get("error").and_then(|v| v.as_i64()).unwrap_or(0);
        if msg_error != 0 {
            return Err(AppError::imessage_error_with_data(
                "Message sent with an error. See attached message",
                data,
            ));
        }

        return Ok(Json(success_response_with_message("Message sent!", data)));
    }

    // AppleScript send — tempGuid and message are required
    if body.temp_guid.as_deref().is_none_or(|s| s.is_empty()) {
        return Err(AppError::bad_request(
            "A 'tempGuid' is required when sending via AppleScript",
        ));
    }
    if message.is_empty() {
        return Err(AppError::bad_request(
            "A 'message' is required when sending via AppleScript",
        ));
    }

    if !state.config.enable_private_api {
        let _ = imessage_apple::actions::start_messages().await;
    }

    // Record the max ROWID BEFORE sending so we can find the new message
    let normalized_guid = normalize_chat_guid(&body.chat_guid);
    let pre_send_max_rowid = {
        let (msgs, _) = state
            .imessage_repo
            .lock()
            .get_messages(&MessageQueryParams {
                chat_guid: Some(normalized_guid.clone()),
                limit: 1,
                sort: SortOrder::Desc,
                ..Default::default()
            })
            .unwrap_or_default();
        msgs.first().map(|m| m.rowid).unwrap_or(0)
    };

    let v = macos_version();
    let format_address = |addr: &str| -> String { addr.to_string() };

    imessage_apple::actions::send_message(
        &body.chat_guid,
        &message,
        "", // no attachment
        false,
        v,
        &format_address,
    )
    .await
    .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    let repo = state.imessage_repo.clone();
    let sent_message = awaiter::await_message(Duration::from_secs(30), || {
        let repo = repo.clone();
        let guid = normalized_guid.clone();
        async move {
            let (msgs, _) = repo
                .lock()
                .get_messages(&MessageQueryParams {
                    chat_guid: Some(guid),
                    limit: 5,
                    sort: SortOrder::Desc,
                    ..Default::default()
                })
                .unwrap_or_default();
            msgs.into_iter()
                .find(|m| m.rowid > pre_send_max_rowid && m.is_from_me)
        }
    })
    .await;

    let msg = sent_message.ok_or_else(|| {
        AppError::imessage_error(
            "Failed to send message! Message not found in database after 30 seconds!",
        )
    })?;

    let msg_config = MessageSerializerConfig::for_sent_message();
    let att_config = AttachmentSerializerConfig::default();
    let mut data = serialize_message(&msg, &msg_config, &att_config, false);
    inject_temp_guid(&mut data, body.temp_guid.as_deref());

    Ok(Json(success_response_with_message("Message sent!", data)))
}

/// POST /api/v1/message/attachment
pub async fn send_attachment(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<Value>, AppError> {
    let mut chat_guid: Option<String> = None;
    let mut temp_guid: Option<String> = None;
    let mut file_name: Option<String> = None;
    let mut file_data: Option<Vec<u8>> = None;
    let mut method: Option<String> = None;
    let mut is_audio_message = false;
    let mut subject: Option<String> = None;
    let mut effect_id: Option<String> = None;
    let mut selected_message_guid: Option<String> = None;
    let mut part_index: Option<i64> = None;
    let mut attributed_body: Option<Value> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "chatGuid" => {
                chat_guid = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "tempGuid" => {
                temp_guid = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "name" => {
                file_name = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "method" => {
                method = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "isAudioMessage" => {
                let val = field
                    .text()
                    .await
                    .map_err(|e| AppError::bad_request(&e.to_string()))?;
                is_audio_message = val == "true" || val == "1";
            }
            "subject" => {
                subject = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "effectId" => {
                effect_id = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "selectedMessageGuid" => {
                selected_message_guid = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "partIndex" => {
                let val = field
                    .text()
                    .await
                    .map_err(|e| AppError::bad_request(&e.to_string()))?;
                part_index = val.parse::<i64>().ok();
            }
            "attributedBody" => {
                let val = field
                    .text()
                    .await
                    .map_err(|e| AppError::bad_request(&e.to_string()))?;
                attributed_body = serde_json::from_str(&val).ok();
            }
            "attachment" => {
                if file_name.is_none() {
                    file_name = field.file_name().map(|s| s.to_string());
                }
                file_data = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| AppError::server_error(&format!("Failed to read file: {e}")))?
                        .to_vec(),
                );
            }
            _ => {
                // Skip unknown fields
                let _ = field.bytes().await;
            }
        }
    }

    let chat_guid = chat_guid.ok_or_else(|| AppError::bad_request("chatGuid is required"))?;
    let data = file_data.ok_or_else(|| AppError::bad_request("attachment file is required"))?;
    if data.is_empty() {
        return Err(AppError::bad_request("Attachment file is empty"));
    }
    let name = sanitize_filename(file_name.as_deref().unwrap_or("attachment"), "attachment");

    check_send_cache(&state, temp_guid.as_deref())?;
    let _cache_guard = SendCacheGuard::new(&state, temp_guid.as_deref());

    // Auto-stop typing indicator if one was active for this chat
    if state.typing_cache.lock().remove(&chat_guid)
        && let Ok(api) = state.require_private_api()
    {
        let _ = api.send_action(actions::stop_typing(&chat_guid)).await;
    }

    // Force private-api when features require it, even if client specified apple-script.
    let needs_private_api = subject.is_some()
        || effect_id.is_some()
        || selected_message_guid.is_some()
        || attributed_body.is_some();

    let resolved_method = if needs_private_api {
        "private-api"
    } else {
        resolve_send_method(method.as_deref(), state.config.enable_private_api)?
    };
    let uuid_str = uuid::Uuid::new_v4().to_string();
    let att_dir = AppPaths::messages_attachments_dir().join(&uuid_str);
    std::fs::create_dir_all(&att_dir)
        .map_err(|e| AppError::server_error(&format!("Failed to create directory: {e}")))?;

    let dest = att_dir.join(&name);
    std::fs::write(&dest, &data)
        .map_err(|e| AppError::server_error(&format!("Failed to write file: {e}")))?;

    let attachment_path = dest.to_string_lossy().to_string();

    // Convert MP3 to CAF for audio messages
    let final_path = if is_audio_message && attachment_path.ends_with(".mp3") {
        let caf_path = attachment_path.replace(".mp3", ".caf");
        match imessage_apple::process::convert_mp3_to_caf(&attachment_path, &caf_path).await {
            Ok(_) => caf_path,
            Err(_) => attachment_path,
        }
    } else {
        attachment_path
    };

    if resolved_method == "private-api" {
        let api = state.require_private_api()?;
        let opts = actions::SendOptions {
            subject: subject.as_deref(),
            effect_id: effect_id.as_deref(),
            selected_message_guid: selected_message_guid.as_deref(),
            part_index,
            attributed_body: attributed_body.as_ref(),
        };
        let action = actions::send_attachment(&chat_guid, &final_path, is_audio_message, &opts);
        let data = send_and_await_private_api(&state, &api, action, true, "attachment").await?;

        let msg_error = data.get("error").and_then(|v| v.as_i64()).unwrap_or(0);
        if msg_error != 0 {
            return Err(AppError::imessage_error_with_data(
                "Attachment sent with an error. See attached message",
                data,
            ));
        }

        let mut data = data;
        inject_temp_guid(&mut data, temp_guid.as_deref());
        return Ok(Json(success_response_with_message(
            "Attachment sent!",
            data,
        )));
    }

    // AppleScript send — tempGuid is required for AppleScript attachment sends
    if temp_guid.as_deref().is_none_or(|s| s.is_empty()) {
        return Err(AppError::bad_request(
            "A 'tempGuid' is required when sending via AppleScript",
        ));
    }

    if !state.config.enable_private_api {
        let _ = imessage_apple::actions::start_messages().await;
    }

    // Record the max ROWID BEFORE sending so we can find the new message
    let normalized_guid = normalize_chat_guid(&chat_guid);
    let pre_send_max_rowid = {
        let (msgs, _) = state
            .imessage_repo
            .lock()
            .get_messages(&MessageQueryParams {
                chat_guid: Some(normalized_guid.clone()),
                limit: 1,
                sort: SortOrder::Desc,
                ..Default::default()
            })
            .unwrap_or_default();
        msgs.first().map(|m| m.rowid).unwrap_or(0)
    };

    // Send via AppleScript (after recording rowid so we don't miss the new message)
    let v = macos_version();
    let format_address = |addr: &str| -> String { addr.to_string() };
    imessage_apple::actions::send_message(
        &chat_guid,
        "",
        &final_path,
        is_audio_message,
        v,
        &format_address,
    )
    .await
    .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    let repo = state.imessage_repo.clone();
    let sent_message = awaiter::await_message(Duration::from_secs(30), || {
        let repo = repo.clone();
        let guid = normalized_guid.clone();
        async move {
            let (msgs, _) = repo
                .lock()
                .get_messages(&MessageQueryParams {
                    chat_guid: Some(guid),
                    limit: 5,
                    sort: SortOrder::Desc,
                    with_attachments: true,
                    ..Default::default()
                })
                .unwrap_or_default();
            msgs.into_iter()
                .find(|m| m.rowid > pre_send_max_rowid && m.is_from_me)
        }
    })
    .await;

    let msg = sent_message.ok_or_else(|| {
        AppError::imessage_error(
            "Failed to send attachment! Message not found in database after 30 seconds!",
        )
    })?;

    let msg_config = MessageSerializerConfig::for_sent_message();
    let att_config = AttachmentSerializerConfig::default();
    let mut data = serialize_message(&msg, &msg_config, &att_config, false);
    inject_temp_guid(&mut data, temp_guid.as_deref());

    Ok(Json(success_response_with_message(
        "Attachment sent!",
        data,
    )))
}

/// POST /api/v1/message/attachment/chunk
pub async fn send_attachment_chunk(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<Value>, AppError> {
    let mut chat_guid: Option<String> = None;
    let mut attachment_guid: Option<String> = None;
    let mut file_name: Option<String> = None;
    let mut chunk_index: Option<u32> = None;
    let mut total_chunks: Option<u32> = None;
    let mut is_complete = false;
    let mut chunk_data: Option<Vec<u8>> = None;
    // Optional params passed through to final send
    let mut method: Option<String> = None;
    let mut is_audio_message = false;
    let mut subject: Option<String> = None;
    let mut effect_id: Option<String> = None;
    let mut selected_message_guid: Option<String> = None;
    let mut part_index: Option<i64> = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "chatGuid" => {
                chat_guid = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "attachmentGuid" => {
                attachment_guid = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "name" => {
                file_name = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "chunkIndex" => {
                let val = field
                    .text()
                    .await
                    .map_err(|e| AppError::bad_request(&e.to_string()))?;
                chunk_index = Some(
                    val.parse::<u32>()
                        .map_err(|_| AppError::bad_request("chunkIndex must be a number"))?,
                );
            }
            "totalChunks" => {
                let val = field
                    .text()
                    .await
                    .map_err(|e| AppError::bad_request(&e.to_string()))?;
                total_chunks = Some(
                    val.parse::<u32>()
                        .map_err(|_| AppError::bad_request("totalChunks must be a number"))?,
                );
            }
            "isComplete" => {
                let val = field
                    .text()
                    .await
                    .map_err(|e| AppError::bad_request(&e.to_string()))?;
                is_complete = val == "true" || val == "1";
            }
            "method" => {
                method = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "isAudioMessage" => {
                let val = field
                    .text()
                    .await
                    .map_err(|e| AppError::bad_request(&e.to_string()))?;
                is_audio_message = val == "true" || val == "1";
            }
            "subject" => {
                subject = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "effectId" => {
                effect_id = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "selectedMessageGuid" => {
                selected_message_guid = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::bad_request(&e.to_string()))?,
                );
            }
            "partIndex" => {
                let val = field
                    .text()
                    .await
                    .map_err(|e| AppError::bad_request(&e.to_string()))?;
                part_index = val.parse::<i64>().ok();
            }
            "chunk" | "attachment" => {
                chunk_data = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| AppError::server_error(&format!("Failed to read chunk: {e}")))?
                        .to_vec(),
                );
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let _chat_guid = chat_guid.ok_or_else(|| AppError::bad_request("chatGuid is required"))?;
    let att_guid_raw =
        attachment_guid.ok_or_else(|| AppError::bad_request("attachmentGuid is required"))?;
    let att_guid = sanitize_path_component(&att_guid_raw, "attachmentGuid")?;
    let name = sanitize_filename(
        file_name
            .as_deref()
            .ok_or_else(|| AppError::bad_request("name is required"))?,
        "attachment",
    );
    let idx = chunk_index.ok_or_else(|| AppError::bad_request("chunkIndex is required"))?;
    let total = total_chunks.ok_or_else(|| AppError::bad_request("totalChunks is required"))?;
    let data = chunk_data.ok_or_else(|| AppError::bad_request("chunk data is required"))?;

    if data.is_empty() {
        return Err(AppError::bad_request("Chunk data is empty"));
    }

    // Validate chunkIndex < totalChunks
    if idx >= total {
        return Err(AppError::bad_request(
            "chunkIndex must be less than totalChunks",
        ));
    }

    // Save chunk to disk
    let chunks_dir = AppPaths::user_data()
        .join("Attachments")
        .join("chunks")
        .join(&att_guid);
    std::fs::create_dir_all(&chunks_dir)
        .map_err(|e| AppError::server_error(&format!("Failed to create chunks directory: {e}")))?;

    let chunk_path = chunks_dir.join(format!("{idx}-{name}"));
    std::fs::write(&chunk_path, &data)
        .map_err(|e| AppError::server_error(&format!("Failed to write chunk: {e}")))?;

    if !is_complete {
        let remaining = total.saturating_sub(idx + 1);
        return Ok(Json(success_response_with_message(
            &format!("Chunk {idx}/{total} uploaded successfully."),
            json!({
                "attachmentGuid": att_guid,
                "chunkIndex": idx,
                "totalChunks": total,
                "remainingChunks": remaining,
            }),
        )));
    }

    // Assembly + send: read all chunks, concatenate, send
    let chat_guid_str = _chat_guid;

    // Auto-stop typing indicator if one was active for this chat
    if state.typing_cache.lock().remove(&chat_guid_str)
        && let Ok(api) = state.require_private_api()
    {
        let _ = api.send_action(actions::stop_typing(&chat_guid_str)).await;
    }

    // Read all chunk files, sort by index, concatenate
    let mut chunk_files: Vec<(u32, std::path::PathBuf)> = Vec::new();
    let entries = std::fs::read_dir(&chunks_dir)
        .map_err(|e| AppError::server_error(&format!("Failed to read chunks directory: {e}")))?;

    for entry in entries {
        let entry =
            entry.map_err(|e| AppError::server_error(&format!("Failed to read entry: {e}")))?;
        let fname = entry.file_name().to_string_lossy().to_string();
        // Filename format: {idx}-{name}
        if let Some(idx_str) = fname.split('-').next()
            && let Ok(idx) = idx_str.parse::<u32>()
        {
            chunk_files.push((idx, entry.path()));
        }
    }

    chunk_files.sort_by_key(|(idx, _)| *idx);

    // Verify all chunks are present before assembly
    if chunk_files.len() != total as usize {
        return Err(AppError::bad_request(&format!(
            "Expected {} chunks but only found {}",
            total,
            chunk_files.len()
        )));
    }

    let mut assembled_data = Vec::new();
    for (_, path) in &chunk_files {
        let chunk_bytes = std::fs::read(path)
            .map_err(|e| AppError::server_error(&format!("Failed to read chunk: {e}")))?;
        assembled_data.extend(chunk_bytes);
    }

    if assembled_data.is_empty() {
        return Err(AppError::server_error("Assembled file is empty"));
    }

    // Save assembled file in ~/Library/Messages/Attachments/imessage-rs/
    let uuid_str = uuid::Uuid::new_v4().to_string();
    let att_dir = AppPaths::messages_attachments_dir().join(&uuid_str);
    std::fs::create_dir_all(&att_dir)
        .map_err(|e| AppError::server_error(&format!("Failed to create directory: {e}")))?;

    let dest = att_dir.join(&name);
    std::fs::write(&dest, &assembled_data)
        .map_err(|e| AppError::server_error(&format!("Failed to write assembled file: {e}")))?;

    let final_path = dest.to_string_lossy().to_string();

    // Clean up chunks directory
    let _ = std::fs::remove_dir_all(&chunks_dir);

    // Force private-api when features require it (matching send_text logic)
    let needs_private_api =
        effect_id.is_some() || subject.is_some() || selected_message_guid.is_some();
    let resolved_method = if needs_private_api {
        "private-api"
    } else {
        resolve_send_method(method.as_deref(), state.config.enable_private_api)?
    };

    if resolved_method == "private-api" {
        let api = state.require_private_api()?;
        let opts = actions::SendOptions {
            subject: subject.as_deref(),
            effect_id: effect_id.as_deref(),
            selected_message_guid: selected_message_guid.as_deref(),
            part_index,
            attributed_body: None,
        };
        let action = actions::send_attachment(&chat_guid_str, &final_path, is_audio_message, &opts);
        let data = send_and_await_private_api(&state, &api, action, true, "attachment").await?;
        return Ok(Json(success_response_with_message(
            "Attachment sent!",
            data,
        )));
    }

    // AppleScript fallback
    if !state.config.enable_private_api {
        let _ = imessage_apple::actions::start_messages().await;
    }

    // Record the max ROWID BEFORE sending so we can find the new message
    let normalized_guid = normalize_chat_guid(&chat_guid_str);
    let pre_send_max_rowid = {
        let (msgs, _) = state
            .imessage_repo
            .lock()
            .get_messages(&MessageQueryParams {
                chat_guid: Some(normalized_guid.clone()),
                limit: 1,
                sort: SortOrder::Desc,
                ..Default::default()
            })
            .unwrap_or_default();
        msgs.first().map(|m| m.rowid).unwrap_or(0)
    };

    // Send via AppleScript (after recording rowid so we don't miss the new message)
    let v = macos_version();
    let format_address = |addr: &str| -> String { addr.to_string() };
    imessage_apple::actions::send_message(
        &chat_guid_str,
        "",
        &final_path,
        is_audio_message,
        v,
        &format_address,
    )
    .await
    .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    let repo = state.imessage_repo.clone();
    let sent_message = awaiter::await_message(Duration::from_secs(30), || {
        let repo = repo.clone();
        let guid = normalized_guid.clone();
        async move {
            let (msgs, _) = repo
                .lock()
                .get_messages(&MessageQueryParams {
                    chat_guid: Some(guid),
                    limit: 5,
                    sort: SortOrder::Desc,
                    with_attachments: true,
                    ..Default::default()
                })
                .unwrap_or_default();
            msgs.into_iter()
                .find(|m| m.rowid > pre_send_max_rowid && m.is_from_me)
        }
    })
    .await;

    let msg = sent_message.ok_or_else(|| {
        AppError::imessage_error(
            "Failed to send attachment! Message not found in database after 30 seconds!",
        )
    })?;

    let msg_config = MessageSerializerConfig::for_sent_message();
    let att_config = AttachmentSerializerConfig::default();
    let data = serialize_message(&msg, &msg_config, &att_config, false);

    Ok(Json(success_response_with_message(
        "Attachment sent!",
        data,
    )))
}

/// POST /api/v1/message/multipart body [Private API only]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMultipartBody {
    pub chat_guid: String,
    pub temp_guid: Option<String>,
    pub parts: Vec<MultipartPart>,
    pub effect_id: Option<String>,
    pub subject: Option<String>,
    pub selected_message_guid: Option<String>,
    pub part_index: Option<u32>,
    pub dd_scan: Option<bool>,
    pub attributed_body: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultipartPart {
    pub part_index: u32,
    pub text: Option<String>,
    pub attachment: Option<String>,
    pub name: Option<String>,
    pub mention: Option<Value>,
}

/// POST /api/v1/message/multipart [Private API required]
pub async fn send_multipart(
    State(state): State<AppState>,
    AppJson(body): AppJson<SendMultipartBody>,
) -> Result<Json<Value>, AppError> {
    if body.chat_guid.is_empty() {
        return Err(AppError::bad_request("chatGuid is required"));
    }
    if body.parts.is_empty() {
        return Err(AppError::bad_request(
            "parts array is required and must not be empty",
        ));
    }

    // Validate parts
    for part in &body.parts {
        if part.text.is_none() && part.attachment.is_none() {
            return Err(AppError::bad_request(
                "Each part must have either text or attachment",
            ));
        }
        if part.attachment.is_some() && part.name.is_none() {
            return Err(AppError::bad_request(
                "name is required when attachment is provided",
            ));
        }
    }

    check_send_cache(&state, body.temp_guid.as_deref())?;
    let _cache_guard = SendCacheGuard::new(&state, body.temp_guid.as_deref());

    // Auto-stop typing indicator if one was active for this chat
    if state.typing_cache.lock().remove(&body.chat_guid)
        && let Ok(api) = state.require_private_api()
    {
        let _ = api.send_action(actions::stop_typing(&body.chat_guid)).await;
    }

    let api = state.require_private_api()?;

    // Build parts as JSON value, copying attachment files into the Messages sandbox
    let mut parts_values: Vec<Value> = Vec::new();
    for p in &body.parts {
        let mut part = json!({
            "partIndex": p.part_index,
        });
        if let Some(ref t) = p.text {
            part["text"] = json!(t);
        }
        if let Some(ref a) = p.attachment {
            // Copy attachment into Messages attachments directory
            // Resolve relative paths against the attachment dir
            let src = if std::path::Path::new(a).is_absolute() || a.starts_with('~') {
                imessage_core::utils::expand_tilde(a)
            } else {
                resolve_relative_path_in_base(
                    &imessage_core::config::AppPaths::messages_attachments_dir(),
                    a,
                    "attachment",
                )?
            };
            if !src.exists() {
                return Err(AppError::bad_request(&format!(
                    "Attachment file does not exist: {a}"
                )));
            }
            let file_name = sanitize_filename(
                p.name
                    .as_deref()
                    .or_else(|| src.file_name().and_then(|n| n.to_str()))
                    .unwrap_or("attachment"),
                "attachment",
            );
            let uuid_str = uuid::Uuid::new_v4().to_string();
            let dest_dir =
                imessage_core::config::AppPaths::messages_attachments_dir().join(&uuid_str);
            std::fs::create_dir_all(&dest_dir)
                .map_err(|e| AppError::server_error(&format!("Failed to create directory: {e}")))?;
            let dest = dest_dir.join(file_name);
            std::fs::copy(&src, &dest)
                .map_err(|e| AppError::server_error(&format!("Failed to copy attachment: {e}")))?;
            part["attachment"] = json!(dest.to_string_lossy());
        }
        if let Some(ref n) = p.name {
            part["name"] = json!(n);
        }
        if let Some(ref m) = p.mention {
            part["mention"] = m.clone();
        }
        parts_values.push(part);
    }
    let parts_json: Value = json!(parts_values);

    let opts = actions::SendOptions {
        subject: body.subject.as_deref(),
        effect_id: body.effect_id.as_deref(),
        selected_message_guid: body.selected_message_guid.as_deref(),
        part_index: body.part_index.map(|p| p as i64),
        attributed_body: body.attributed_body.as_ref(),
    };
    let action = actions::send_multipart(&body.chat_guid, &parts_json, &opts, body.dd_scan);
    let mut data = send_and_await_private_api(&state, &api, action, true, "message").await?;
    inject_temp_guid(&mut data, body.temp_guid.as_deref());

    let msg_error = data.get("error").and_then(|v| v.as_i64()).unwrap_or(0);
    if msg_error != 0 {
        return Err(AppError::imessage_error_with_data(
            "Message sent with an error. See attached message",
            data,
        ));
    }

    Ok(Json(success_response_with_message("Message sent!", data)))
}

/// Decode a data URL into bytes, write to a temp sticker file, return the path.
/// Format: "data:<mime>;base64,<data>" → writes sticker.<ext> under messages attachments dir.
fn decode_sticker_data_url(data_url: &str) -> Result<String, AppError> {
    use base64::Engine;

    let rest = data_url.strip_prefix("data:").ok_or_else(|| {
        AppError::bad_request("Invalid sticker data URL: must start with 'data:'")
    })?;
    let (mime, b64_data) = rest.split_once(";base64,").ok_or_else(|| {
        AppError::bad_request("Invalid sticker data URL: expected ';base64,' separator")
    })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64_data)
        .map_err(|e| AppError::bad_request(&format!("Invalid sticker base64 data: {e}")))?;

    if bytes.is_empty() {
        return Err(AppError::bad_request("Sticker data is empty"));
    }

    if !mime.starts_with("image/") {
        return Err(AppError::bad_request(&format!(
            "Invalid sticker MIME type: \"{mime}\". Must be an image type (e.g. image/png, image/jpeg)."
        )));
    }

    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/heic" => "heic",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/tiff" => "tiff",
        _ => "png", // unknown image subtype — default to png
    };

    let dir = AppPaths::messages_attachments_dir().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::server_error(&format!("Failed to create sticker dir: {e}")))?;
    let path = dir.join(format!("sticker.{ext}"));
    std::fs::write(&path, &bytes)
        .map_err(|e| AppError::server_error(&format!("Failed to write sticker file: {e}")))?;

    Ok(path.to_string_lossy().to_string())
}

/// POST /api/v1/message/react body [Private API required]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendReactionBody {
    pub chat_guid: String,
    pub selected_message_guid: String,
    pub reaction: String,
    pub part_index: Option<u32>,
    /// Data URL for sticker reactions (e.g. "data:image/png;base64,iVBOR...")
    pub sticker: Option<String>,
}

/// POST /api/v1/message/react [Private API required]
pub async fn send_reaction(
    State(state): State<AppState>,
    AppJson(body): AppJson<SendReactionBody>,
) -> Result<Json<Value>, AppError> {
    if body.chat_guid.is_empty() {
        return Err(AppError::bad_request("chatGuid is required"));
    }
    if body.selected_message_guid.is_empty() {
        return Err(AppError::bad_request("selectedMessageGuid is required"));
    }

    if body.reaction.is_empty() {
        return Err(AppError::bad_request("reaction is required"));
    }

    let classic_reactions = [
        "love",
        "like",
        "dislike",
        "laugh",
        "emphasize",
        "question",
        "-love",
        "-like",
        "-dislike",
        "-laugh",
        "-emphasize",
        "-question",
    ];

    // Determine reaction category and build optional params
    let is_classic = classic_reactions.contains(&body.reaction.as_str());
    let is_sticker = body.reaction == "sticker" || body.reaction == "-sticker";

    // For sticker-add, require the data URL; for sticker-remove, pass empty path
    let sticker_path = if body.reaction == "sticker" {
        let data_url = body.sticker.as_deref().ok_or_else(|| {
            AppError::bad_request("sticker data URL is required for sticker reactions")
        })?;
        Some(decode_sticker_data_url(data_url)?)
    } else if body.reaction == "-sticker" {
        Some(String::new())
    } else {
        None
    };

    // For emoji reactions, extract and validate the emoji string
    let emoji = if !is_classic && !is_sticker {
        let raw = body.reaction.strip_prefix('-').unwrap_or(&body.reaction);
        if emojis::get(raw).is_none() {
            return Err(AppError::bad_request(&format!(
                "Invalid reaction: \"{}\". Must be a classic reaction ({}), \"sticker\", or a single emoji.",
                body.reaction,
                classic_reactions[..6].join(", ")
            )));
        }
        Some(raw.to_string())
    } else {
        None
    };

    // Verify the target message exists
    {
        let msg = state
            .imessage_repo
            .lock()
            .get_message(&body.selected_message_guid, false, false)
            .map_err(|e| AppError::server_error(&e.to_string()))?;
        if msg.is_none() {
            return Err(AppError::bad_request(&format!(
                "Message with GUID \"{}\" does not exist!",
                body.selected_message_guid
            )));
        }
    }

    let api = state.require_private_api()?;
    let action = actions::send_reaction(
        &body.chat_guid,
        &body.selected_message_guid,
        &body.reaction,
        body.part_index.map(|p| p as i64),
        emoji.as_deref(),
        sticker_path.as_deref(),
    );
    let result = api
        .send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    let txn = result.ok_or_else(|| AppError::imessage_error("Failed to send reaction!"))?;
    if txn.identifier.is_empty() {
        return Err(AppError::imessage_error("Failed to send reaction!"));
    }

    // Poll DB with exponential backoff until message appears (60s max)
    let identifier = txn.identifier.clone();
    let repo = state.imessage_repo.clone();
    let msg = awaiter::await_message(Duration::from_secs(60), || {
        let repo = repo.clone();
        let id = identifier.clone();
        async move { repo.lock().get_message(&id, false, false).ok().flatten() }
    })
    .await
    .ok_or_else(|| {
        AppError::imessage_error(
            "Failed to send reaction! Message not found in database after 60 seconds!",
        )
    })?;

    let msg_config = MessageSerializerConfig::for_sent_message();
    let att_config = AttachmentSerializerConfig::default();
    let data = serialize_message(&msg, &msg_config, &att_config, false);

    Ok(Json(success_response_with_message("Reaction sent!", data)))
}

/// POST /api/v1/message/:guid/edit body [Private API required]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditMessageBody {
    pub edited_message: String,
    pub backwards_compatibility_message: String,
    pub part_index: Option<u32>,
}

/// POST /api/v1/message/:guid/edit [Private API required]
pub async fn edit_message(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    AppJson(body): AppJson<EditMessageBody>,
) -> Result<Json<Value>, AppError> {
    if body.edited_message.is_empty() {
        return Err(AppError::bad_request("editedMessage is required"));
    }
    if body.backwards_compatibility_message.is_empty() {
        return Err(AppError::bad_request(
            "backwardsCompatibilityMessage is required",
        ));
    }

    let api = state.require_private_api()?;

    // Look up the message to get its chatGuid and current dateEdited
    let message = state
        .imessage_repo
        .lock()
        .get_message(&guid, true, false)
        .map_err(|e| AppError::server_error(&e.to_string()))?
        .ok_or_else(|| AppError::bad_request("Selected message does not exist!"))?;

    let current_date_edited = message.date_edited.unwrap_or(0);

    let chat_guid = message
        .chats
        .first()
        .map(|c| c.guid.as_str())
        .ok_or_else(|| AppError::bad_request("Associated chat not found!"))?;

    let action = actions::edit_message(
        chat_guid,
        &guid,
        &body.edited_message,
        &body.backwards_compatibility_message,
        body.part_index.map(|p| p as i64),
    );
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    // Poll until dateEdited changes (30s max)
    let guid_clone = guid.clone();
    let repo = state.imessage_repo.clone();
    let msg = awaiter::await_condition(
        Duration::from_secs(30),
        || {
            let repo = repo.clone();
            let g = guid_clone.clone();
            async move { repo.lock().get_message(&g, true, false).ok().flatten() }
        },
        |m| m.date_edited.unwrap_or(0) > current_date_edited,
    )
    .await
    .ok_or_else(|| {
        AppError::imessage_error("Failed to edit message! Message not edited after 30 seconds!")
    })?;

    let msg_config = MessageSerializerConfig::for_sent_message();
    let att_config = AttachmentSerializerConfig::default();
    let data = serialize_message(&msg, &msg_config, &att_config, false);

    Ok(Json(success_response_with_message("Message edited!", data)))
}

/// POST /api/v1/message/:guid/unsend body [Private API required]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsendMessageBody {
    pub part_index: Option<u32>,
}

/// POST /api/v1/message/:guid/unsend [Private API required]
pub async fn unsend_message(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    AppJson(body): AppJson<UnsendMessageBody>,
) -> Result<Json<Value>, AppError> {
    let api = state.require_private_api()?;

    // Look up the message to get its chatGuid and current dateEdited
    let message = state
        .imessage_repo
        .lock()
        .get_message(&guid, true, false)
        .map_err(|e| AppError::server_error(&e.to_string()))?
        .ok_or_else(|| AppError::bad_request("Selected message does not exist!"))?;

    let current_date_edited = message.date_edited.unwrap_or(0);

    let chat_guid = message
        .chats
        .first()
        .map(|c| c.guid.as_str())
        .ok_or_else(|| AppError::bad_request("Associated chat not found!"))?;

    let action = actions::unsend_message(chat_guid, &guid, body.part_index.map(|p| p as i64));
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    // Poll until dateEdited changes (30s max)
    let guid_clone = guid.clone();
    let repo = state.imessage_repo.clone();
    let msg = awaiter::await_condition(
        Duration::from_secs(30),
        || {
            let repo = repo.clone();
            let g = guid_clone.clone();
            async move { repo.lock().get_message(&g, true, false).ok().flatten() }
        },
        |m| m.date_edited.unwrap_or(0) > current_date_edited,
    )
    .await
    .ok_or_else(|| {
        AppError::imessage_error(
            "Failed to unsend message! Message not edited (unsent) after 30 seconds!",
        )
    })?;

    let msg_config = MessageSerializerConfig::for_sent_message();
    let att_config = AttachmentSerializerConfig::default();
    let data = serialize_message(&msg, &msg_config, &att_config, false);

    Ok(Json(success_response_with_message("Message unsent!", data)))
}

/// POST /api/v1/message/:guid/notify [Private API required]
pub async fn notify_message(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let api = state.require_private_api()?;

    // Look up the message to get its chatGuid and check didNotifyRecipient
    let message = state
        .imessage_repo
        .lock()
        .get_message(&guid, true, false)
        .map_err(|e| AppError::server_error(&e.to_string()))?
        .ok_or_else(|| AppError::bad_request("Selected message does not exist!"))?;

    if message.did_notify_recipient.unwrap_or(false) {
        return Err(AppError::bad_request(
            "The recipient has already been notified of this message!",
        ));
    }

    let chat_guid = message
        .chats
        .first()
        .map(|c| c.guid.as_str())
        .ok_or_else(|| AppError::bad_request("Associated chat not found!"))?;

    let action = actions::notify_silenced(chat_guid, &guid);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    // Poll until didNotifyRecipient becomes true (30s max)
    let guid_clone = guid.clone();
    let repo = state.imessage_repo.clone();
    let msg = awaiter::await_condition(
        Duration::from_secs(30),
        || {
            let repo = repo.clone();
            let g = guid_clone.clone();
            async move { repo.lock().get_message(&g, true, false).ok().flatten() }
        },
        |m| m.did_notify_recipient.unwrap_or(false),
    )
    .await;

    // Don't throw on timeout — return whatever we have
    let data = if let Some(msg) = msg {
        let msg_config = MessageSerializerConfig::for_sent_message();
        let att_config = AttachmentSerializerConfig::default();
        serialize_message(&msg, &msg_config, &att_config, false)
    } else {
        json!(null)
    };

    Ok(Json(success_response(data)))
}

/// GET /api/v1/message/:guid/embedded-media [Private API required]
/// Returns the binary media file (digital touch, handwriting, etc.)
pub async fn get_embedded_media(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<axum::response::Response, AppError> {
    let api = state.require_private_api()?;

    // Look up the message to get its chatGuid
    let message = state
        .imessage_repo
        .lock()
        .get_message(&guid, true, false)
        .map_err(|e| AppError::server_error(&e.to_string()))?
        .ok_or_else(|| AppError::not_found("Message does not exist!"))?;

    // Validate that the message has a supported balloonBundleId (digital touch or handwriting)
    let bundle_id = message.balloon_bundle_id.as_deref().unwrap_or("");
    let is_digital_touch = bundle_id == "com.apple.DigitalTouchBalloonProvider";
    let is_handwriting = bundle_id == "com.apple.Handwriting.HandwritingProvider";
    if !is_digital_touch && !is_handwriting {
        return Err(AppError::bad_request(
            "Message does not have embedded media!",
        ));
    }

    let chat_guid = message
        .chats
        .first()
        .map(|c| c.guid.as_str())
        .ok_or_else(|| AppError::bad_request("Associated chat not found!"))?;

    let action = actions::get_embedded_media(chat_guid, &guid);
    let result = api
        .send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    let data = if let Some(txn) = result {
        txn.data.unwrap_or(json!(null))
    } else {
        json!(null)
    };

    // Extract the path from the Private API response
    let file_path = data
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::server_error("No media path returned from Private API"))?;

    // Strip file:// prefix if present, then expand ~ to home directory
    let clean_path = file_path.strip_prefix("file://").unwrap_or(file_path);
    let path = imessage_core::utils::expand_tilde(clean_path);

    if !path.exists() {
        return Err(AppError::not_found("Embedded media file not found on disk"));
    }

    // Determine MIME type from extension
    let mime_type = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();

    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| AppError::server_error(&format!("Failed to open file: {e}")))?;

    let metadata = file
        .metadata()
        .await
        .map_err(|e| AppError::server_error(&format!("Failed to read metadata: {e}")))?;

    let file_name = sanitize_header_filename(
        path.file_name().and_then(|n| n.to_str()).unwrap_or("media"),
        "media",
    );

    let stream = tokio_util::io::ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);

    let mut response = axum::response::Response::new(body);
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_str(&mime_type)
            .map_err(|e| AppError::server_error(&format!("Invalid content type: {e}")))?,
    );
    headers.insert(
        axum::http::header::CONTENT_LENGTH,
        axum::http::HeaderValue::from_str(&metadata.len().to_string())
            .map_err(|e| AppError::server_error(&format!("Invalid content length: {e}")))?,
    );
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{file_name}\""))
            .map_err(|e| AppError::server_error(&format!("Invalid content disposition: {e}")))?,
    );

    Ok(response)
}
