use axum::Json;
/// Chat routes:
///   GET    /api/v1/chat/count
///   POST   /api/v1/chat/query
///   POST   /api/v1/chat/new
///   GET    /api/v1/chat/:guid/message
///   GET    /api/v1/chat/:guid
///   PUT    /api/v1/chat/:guid
///   DELETE /api/v1/chat/:guid
///   POST   /api/v1/chat/:guid/read
///   POST   /api/v1/chat/:guid/unread
///   POST   /api/v1/chat/:guid/leave
///   POST   /api/v1/chat/:guid/typing
///   DELETE /api/v1/chat/:guid/typing
///   POST   /api/v1/chat/:guid/participant/add
///   POST   /api/v1/chat/:guid/participant/remove
///   POST   /api/v1/chat/:guid/participant
///   DELETE /api/v1/chat/:guid/participant
///   POST   /api/v1/chat/:guid/icon
///   DELETE /api/v1/chat/:guid/icon
///   POST   /api/v1/chat/:guid/share/contact
///   DELETE /api/v1/chat/:guid/:messageGuid
use axum::extract::{Multipart, Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

use imessage_core::config::AppPaths;
use imessage_db::imessage::types::{ChatQueryParams, MessageQueryParams, SortOrder};
use imessage_private_api::actions;
use imessage_serializers::chat::serialize_chat;
use imessage_serializers::config::{
    AttachmentSerializerConfig, ChatSerializerConfig, MessageSerializerConfig,
};
use imessage_serializers::message::serialize_message;

use crate::awaiter;
use crate::extractors::AppJson;
use crate::middleware::error::{
    AppError, success_response, success_response_with_message, success_response_with_metadata,
};
use crate::path_safety::{sanitize_filename, sanitize_header_filename};
use crate::state::AppState;
use crate::validators::{normalize_chat_guid, parse_with_query, with_has};

/// GET /api/v1/chat/count query params
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChatCountParams {
    pub include_archived: Option<String>,
}

/// GET /api/v1/chat/count
pub async fn count(
    State(state): State<AppState>,
    Query(params): Query<ChatCountParams>,
) -> Result<Json<Value>, AppError> {
    let with_archived = params
        .include_archived
        .as_deref()
        .map(is_truthy_bool)
        .unwrap_or(true);

    let (chats, total_count) = state
        .imessage_repo
        .lock()
        .get_chats(&ChatQueryParams {
            with_archived,
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let mut breakdown = serde_json::Map::new();
    for chat in &chats {
        let service = chat.service_name.as_deref().unwrap_or("Unknown");
        let counter = breakdown
            .entry(service.to_string())
            .or_insert_with(|| json!(0));
        if let Some(n) = counter.as_i64() {
            *counter = json!(n + 1);
        }
    }

    Ok(Json(success_response(json!({
        "total": total_count,
        "breakdown": Value::Object(breakdown),
    }))))
}

/// POST /api/v1/chat/query body
#[derive(Debug, Deserialize, Default)]
pub struct ChatQueryBody {
    pub guid: Option<String>,
    #[serde(rename = "with")]
    pub with_query: Option<Vec<String>>,
    pub sort: Option<String>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

/// POST /api/v1/chat/query
pub async fn query(
    State(state): State<AppState>,
    AppJson(body): AppJson<ChatQueryBody>,
) -> Result<Json<Value>, AppError> {
    let with_query: Vec<String> = body
        .with_query
        .unwrap_or_default()
        .iter()
        .map(|s| s.trim().to_lowercase())
        .collect();

    let with_last_message = with_has(&with_query, &["lastmessage", "last-message"]);

    let offset = body.offset.unwrap_or(0);
    let limit = body.limit.unwrap_or(1000).min(1000); // max:1000

    let sort = if with_last_message && body.sort.is_none() {
        Some("lastmessage".to_string())
    } else {
        body.sort
    };

    let (chats, total) = state
        .imessage_repo
        .lock()
        .get_chats(&ChatQueryParams {
            chat_guid: body.guid.clone(),
            with_archived: true,
            with_participants: true,
            offset,
            limit: Some(limit),
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let chat_config = ChatSerializerConfig {
        include_participants: true,
        include_messages: false,
    };
    let att_config = AttachmentSerializerConfig::default();

    let mut results: Vec<Value> = Vec::new();

    for chat in &chats {
        let mut chat_json = serialize_chat(chat, &chat_config, false);

        if with_last_message {
            if let Ok(Some(last_msg)) = state.imessage_repo.lock().get_chat_last_message(&chat.guid)
            {
                // Parse attributed body, message summary, and payload data
                let msg_config = MessageSerializerConfig {
                    parse_attributed_body: true,
                    parse_message_summary: true,
                    parse_payload_data: true,
                    load_chat_participants: false,
                    ..Default::default()
                };
                let msg_json = serialize_message(&last_msg, &msg_config, &att_config, false);
                chat_json
                    .as_object_mut()
                    .unwrap()
                    .insert("lastMessage".to_string(), msg_json);
            } else {
                chat_json
                    .as_object_mut()
                    .unwrap()
                    .insert("lastMessage".to_string(), Value::Null);
            }
        }

        results.push(chat_json);
    }

    if sort.as_deref() == Some("lastmessage") && with_last_message {
        results.sort_by(|a, b| {
            let d1 = a
                .get("lastMessage")
                .and_then(|m| m.get("dateCreated").or_else(|| m.get("dateDelivered")))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let d2 = b
                .get("lastMessage")
                .and_then(|m| m.get("dateCreated").or_else(|| m.get("dateDelivered")))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            d2.cmp(&d1)
        });
    }

    let metadata = json!({
        "count": results.len(),
        "total": total,
        "offset": offset,
        "limit": limit,
    });

    Ok(Json(success_response_with_metadata(
        json!(results),
        metadata,
    )))
}

/// GET /api/v1/chat/:guid query params
#[derive(Debug, Deserialize, Default)]
pub struct ChatFindParams {
    #[serde(rename = "with")]
    pub with_query: Option<String>,
}

/// GET /api/v1/chat/:guid
pub async fn find(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    Query(params): Query<ChatFindParams>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);
    let with_query = parse_with_query(params.with_query.as_deref());
    let with_participants = with_query.contains(&"participants".to_string());
    let with_last_message = with_query.contains(&"lastmessage".to_string());

    let (chats, _) = state
        .imessage_repo
        .lock()
        .get_chats(&ChatQueryParams {
            chat_guid: Some(guid.clone()),
            with_participants,
            with_archived: true,
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let chat = chats
        .first()
        .ok_or_else(|| AppError::not_found("Chat does not exist!"))?;

    let config = ChatSerializerConfig {
        include_participants: true,
        include_messages: false,
    };

    let mut data = serialize_chat(chat, &config, false);
    let att_config = AttachmentSerializerConfig::default();

    if with_last_message {
        if let Ok(Some(last_msg)) = state.imessage_repo.lock().get_chat_last_message(&guid) {
            // Use default config with only loadChatParticipants=false
            let msg_config = MessageSerializerConfig {
                load_chat_participants: false,
                ..Default::default()
            };
            let msg_json = serialize_message(&last_msg, &msg_config, &att_config, false);
            data.as_object_mut()
                .unwrap()
                .insert("lastMessage".to_string(), msg_json);
        } else {
            data.as_object_mut()
                .unwrap()
                .insert("lastMessage".to_string(), Value::Null);
        }
    }

    Ok(Json(success_response(data)))
}

/// GET /api/v1/chat/:guid/message query params
#[derive(Debug, Deserialize, Default)]
pub struct ChatMessagesParams {
    #[serde(rename = "with")]
    pub with_query: Option<String>,
    pub sort: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub offset: Option<String>,
    pub limit: Option<String>,
}

/// GET /api/v1/chat/:guid/message
pub async fn get_messages(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    Query(params): Query<ChatMessagesParams>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);
    let with_query = parse_with_query(params.with_query.as_deref());
    let with_attachments = with_has(&with_query, &["attachment", "attachments"]);
    let with_attributed_body = with_has(
        &with_query,
        &[
            "message.attributedbody",
            "message.attributed-body",
            "attributedbody",
            "attributed-body",
        ],
    );
    let with_message_summary = with_has(
        &with_query,
        &[
            "message.messagesummaryinfo",
            "message.message-summary-info",
            "messagesummaryinfo",
            "message-summary-info",
        ],
    );
    let with_payload_data = with_has(
        &with_query,
        &[
            "message.payloaddata",
            "message.payload-data",
            "payloaddata",
            "payload-data",
        ],
    );

    // Verify chat exists
    {
        let (chats, _) = state
            .imessage_repo
            .lock()
            .get_chats(&ChatQueryParams {
                chat_guid: Some(guid.clone()),
                with_participants: false,
                with_archived: true,
                ..Default::default()
            })
            .map_err(|e| AppError::server_error(&e.to_string()))?;

        if chats.is_empty() {
            return Err(AppError::not_found("Chat does not exist!"));
        }
    }

    let offset = params
        .offset
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let limit = params
        .limit
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(100)
        .min(1000); // max:1000

    let sort_order = match params.sort.as_deref() {
        Some("ASC") => SortOrder::Asc,
        _ => SortOrder::Desc,
    };

    let before = params.before.as_deref().and_then(|s| s.parse::<i64>().ok());
    let after = params.after.as_deref().and_then(|s| s.parse::<i64>().ok());

    let (messages, total_count) = state
        .imessage_repo
        .lock()
        .get_messages(&MessageQueryParams {
            chat_guid: Some(guid),
            with_attachments,
            offset,
            limit,
            sort: sort_order,
            before,
            after,
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let msg_config = MessageSerializerConfig {
        parse_attributed_body: with_attributed_body,
        parse_message_summary: with_message_summary,
        parse_payload_data: with_payload_data,
        load_chat_participants: false,
        ..Default::default()
    };
    let att_config = AttachmentSerializerConfig::default();

    let data: Vec<Value> = messages
        .iter()
        .map(|m| serialize_message(m, &msg_config, &att_config, false))
        .collect();

    let metadata = json!({
        "offset": offset,
        "limit": limit,
        "total": total_count,
        "count": messages.len(),
    });

    Ok(Json(success_response_with_metadata(json!(data), metadata)))
}

fn is_truthy_bool(s: &str) -> bool {
    matches!(s.to_lowercase().as_str(), "true" | "1" | "yes")
}

// ---------------------------------------------------------------------------
// Write routes
// ---------------------------------------------------------------------------

/// POST /api/v1/chat/new body
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChatBody {
    pub addresses: Vec<String>,
    pub message: Option<String>,
    pub method: Option<String>,
    pub service: Option<String>,
    pub temp_guid: Option<String>,
    pub subject: Option<String>,
    pub effect_id: Option<String>,
    pub attributed_body: Option<Value>,
}

/// POST /api/v1/chat/new
pub async fn create(
    State(state): State<AppState>,
    AppJson(body): AppJson<CreateChatBody>,
) -> Result<Json<Value>, AppError> {
    if body.addresses.is_empty() {
        return Err(AppError::bad_request("addresses is required"));
    }

    // Normalize addresses (phone → E.164, email → lowercase)
    let region = imessage_apple::process::get_region()
        .await
        .unwrap_or_else(|_| "US".to_string());
    let addresses: Vec<String> = body
        .addresses
        .iter()
        .map(|a| imessage_core::phone::normalize_address(a, &region))
        .collect();

    let service = body.service.as_deref().unwrap_or("iMessage");
    let method = body.method.as_deref().unwrap_or("apple-script");

    if method == "private-api" {
        if body.message.as_deref().is_none_or(|m| m.is_empty()) {
            return Err(AppError::bad_request(
                "A message is required when creating chats with the Private API!",
            ));
        }

        let api = state.require_private_api()?;

        let action = actions::create_chat(
            &addresses,
            body.message.as_deref().unwrap_or(""),
            service,
            body.attributed_body.as_ref(),
            body.effect_id.as_deref(),
            body.subject.as_deref(),
        );
        let result = api
            .send_action(action)
            .await
            .map_err(|e| AppError::imessage_error(&e.to_string()))?;

        let txn = result.ok_or_else(|| {
            AppError::imessage_error("Failed to create chat via the Private API!")
        })?;
        if txn.identifier.is_empty() {
            return Err(AppError::imessage_error(
                "Failed to create chat via the Private API!",
            ));
        }

        // Transaction returns a MESSAGE GUID - poll for message with chats populated
        let message_guid = txn.identifier.clone();
        let repo = state.imessage_repo.clone();
        let msg = awaiter::result_awaiter(
            Duration::from_millis(500), // 500ms initial wait (longer for chat create)
            1.5,
            Duration::from_secs(30),
            || {
                let repo = repo.clone();
                let guid = message_guid.clone();
                async move {
                    match repo.lock().get_message(&guid, true, false) {
                        Ok(Some(m)) if !m.chats.is_empty() => Some(m),
                        _ => None,
                    }
                }
            },
            |_| true, // any message with chats is satisfactory
        )
        .await
        .ok_or_else(|| {
            AppError::imessage_error(
                "Failed to create new chat! Message not found after 30 seconds!",
            )
        })?;

        // Extract chat GUID from message, re-fetch with participants
        let chat_guid = msg.chats[0].guid.clone();
        let (chats, _) = state
            .imessage_repo
            .lock()
            .get_chats(&ChatQueryParams {
                chat_guid: Some(chat_guid),
                with_participants: true,
                with_archived: true,
                ..Default::default()
            })
            .map_err(|e| AppError::server_error(&e.to_string()))?;

        let chat = chats.first().ok_or_else(|| {
            AppError::imessage_error("Failed to create new chat! Chat not found!")
        })?;

        let config = ChatSerializerConfig {
            include_participants: true,
            include_messages: false,
        };
        let mut data = serialize_chat(chat, &config, false);

        // Inject messages array with the sent message
        let msg_config = MessageSerializerConfig {
            include_chats: false, // includeChats: false for chat.messages
            ..Default::default()
        };
        let att_config = AttachmentSerializerConfig::default();
        let mut msg_json = serialize_message(&msg, &msg_config, &att_config, false);
        if let Some(temp_guid) = &body.temp_guid
            && let Some(obj) = msg_json.as_object_mut()
        {
            obj.insert("tempGuid".to_string(), json!(temp_guid));
        }
        if let Some(obj) = data.as_object_mut() {
            obj.insert("messages".to_string(), json!([msg_json]));
        }

        return Ok(Json(success_response_with_message(
            "Successfully created chat!",
            data,
        )));
    }

    // AppleScript chat creation
    let message = body.message.as_deref();

    // Cannot create group chats via AppleScript
    if addresses.len() > 1 {
        return Err(AppError::imessage_error(
            "Cannot create group chats via AppleScript!",
        ));
    }
    // A message is required when creating chats via AppleScript
    if message.is_none_or(|m| m.is_empty()) {
        return Err(AppError::imessage_error(
            "A message is required when creating chats!",
        ));
    }

    let output = imessage_apple::actions::create_chat(&addresses, service, message)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    // Try to find the created chat
    // The AppleScript output may contain the chat GUID
    let chat_guid = output.trim().to_string();

    if !chat_guid.is_empty() {
        // Poll for the chat to appear in the DB (AppleScript is async)
        let repo = state.imessage_repo.clone();
        let guid = chat_guid.clone();
        let chat = awaiter::await_message(Duration::from_secs(30), || {
            let repo = repo.clone();
            let guid = guid.clone();
            async move {
                let (chats, _) = repo
                    .lock()
                    .get_chats(&ChatQueryParams {
                        chat_guid: Some(guid),
                        with_participants: true,
                        with_archived: true,
                        ..Default::default()
                    })
                    .ok()?;
                chats.into_iter().next()
            }
        })
        .await;

        if let Some(ref chat) = chat {
            let config = ChatSerializerConfig {
                include_participants: true,
                include_messages: false,
            };
            let mut data = serialize_chat(chat, &config, false);

            // Include sent message in response
            if message.is_some() {
                let normalized = normalize_chat_guid(&chat_guid);
                if let Ok((msgs, _)) =
                    state
                        .imessage_repo
                        .lock()
                        .get_messages(&MessageQueryParams {
                            chat_guid: Some(normalized),
                            limit: 1,
                            sort: SortOrder::Desc,
                            ..Default::default()
                        })
                {
                    let msg_config = MessageSerializerConfig {
                        include_chats: false,
                        ..Default::default()
                    };
                    let att_config = AttachmentSerializerConfig::default();
                    let msg_jsons: Vec<Value> = msgs
                        .iter()
                        .map(|m| {
                            let mut mj = serialize_message(m, &msg_config, &att_config, false);
                            if let Some(tg) = &body.temp_guid
                                && let Some(obj) = mj.as_object_mut()
                            {
                                obj.insert("tempGuid".to_string(), json!(tg));
                            }
                            mj
                        })
                        .collect();
                    if let Some(obj) = data.as_object_mut() {
                        obj.insert("messages".to_string(), json!(msg_jsons));
                    }
                }
            }

            return Ok(Json(success_response_with_message(
                "Successfully created chat!",
                data,
            )));
        }
    }

    Err(AppError::server_error(
        "Failed to create new chat! Chat not found in database!",
    ))
}

/// PUT /api/v1/chat/:guid body [Private API required]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateChatBody {
    pub display_name: Option<String>,
}

/// PUT /api/v1/chat/:guid [Private API required]
pub async fn update(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    AppJson(body): AppJson<UpdateChatBody>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);

    // Verify chat exists and is a group chat
    let (chats, _) = state
        .imessage_repo
        .lock()
        .get_chats(&ChatQueryParams {
            chat_guid: Some(guid.clone()),
            with_participants: true,
            with_archived: true,
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let chat = chats
        .first()
        .ok_or_else(|| AppError::not_found("Chat does not exist!"))?;

    let config = ChatSerializerConfig {
        include_participants: true,
        include_messages: false,
    };

    // If displayName is not provided/empty, return 200 with "not updated" message
    let display_name = match body.display_name.as_deref() {
        Some(name) if !name.is_empty() => name,
        _ => {
            let data = serialize_chat(chat, &config, false);
            return Ok(Json(success_response_with_message(
                "Chat not updated! No update information provided!",
                data,
            )));
        }
    };

    if chat.participants.len() <= 1 {
        return Err(AppError::imessage_error("Cannot rename a non-group chat!"));
    }

    let prev_name = chat.display_name.clone().unwrap_or_default();

    // Early return if the name is already the same
    if prev_name == display_name {
        let data = serialize_chat(chat, &config, false);
        return Ok(Json(success_response_with_message(
            "Successfully updated the following fields: displayName",
            data,
        )));
    }

    let api = state.require_private_api()?;
    let action = actions::set_display_name(&guid, display_name);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    // Poll DB until display name changes
    let guid_clone = guid.clone();
    let repo = state.imessage_repo.clone();
    let updated_chat = awaiter::await_condition(
        Duration::from_secs(30),
        || {
            let repo = repo.clone();
            let g = guid_clone.clone();
            async move {
                repo.lock()
                    .get_chats(&ChatQueryParams {
                        chat_guid: Some(g),
                        with_participants: true,
                        with_archived: true,
                        ..Default::default()
                    })
                    .ok()
                    .and_then(|(chats, _)| chats.into_iter().next())
            }
        },
        |c| c.display_name.as_deref().unwrap_or("") != prev_name,
    )
    .await;

    let chat = updated_chat.ok_or_else(|| {
        AppError::imessage_error(
            "Failed to rename chat! Chat name did not change after 30 seconds!",
        )
    })?;

    let data = serialize_chat(&chat, &config, false);

    Ok(Json(success_response_with_message(
        "Successfully updated the following fields: displayName",
        data,
    )))
}

/// DELETE /api/v1/chat/:guid [Private API required]
pub async fn delete_chat(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);

    // Verify chat exists
    {
        let (chats, _) = state
            .imessage_repo
            .lock()
            .get_chats(&ChatQueryParams {
                chat_guid: Some(guid.clone()),
                with_archived: true,
                ..Default::default()
            })
            .map_err(|e| AppError::server_error(&e.to_string()))?;

        if chats.is_empty() {
            return Err(AppError::not_found(&format!(
                "Failed to delete chat! Chat not found. (GUID: {guid})"
            )));
        }
    }

    let api = state.require_private_api()?;
    let action = actions::delete_chat(&guid);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    // Poll DB until chat is gone (30s max)
    let guid_clone = guid.clone();
    let repo = state.imessage_repo.clone();
    let deleted = awaiter::await_condition(
        Duration::from_secs(30),
        || {
            let repo = repo.clone();
            let g = guid_clone.clone();
            async move {
                let (chats, _) = repo
                    .lock()
                    .get_chats(&ChatQueryParams {
                        chat_guid: Some(g),
                        with_archived: true,
                        ..Default::default()
                    })
                    .unwrap_or_default();
                // Return Some(true) when chat is gone, None to keep polling
                if chats.is_empty() { Some(true) } else { None }
            }
        },
        |_| true,
    )
    .await;

    if deleted.is_none() {
        return Err(AppError::imessage_error(&format!(
            "Failed to delete chat! Chat still exists. (GUID: {guid})"
        )));
    }

    Ok(Json(success_response_with_message(
        "Successfully deleted chat!",
        json!(null),
    )))
}

/// POST /api/v1/chat/:guid/read [Private API required]
pub async fn mark_read(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);
    let api = state.require_private_api()?;
    let action = actions::mark_chat_read(&guid);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    // Dispatch webhook event
    if let Some(ref ws) = state.webhook_service {
        ws.dispatch(
            imessage_core::events::CHAT_READ_STATUS_CHANGED,
            json!({ "chatGuid": guid, "read": true }),
            None,
        )
        .await;
    }

    Ok(Json(success_response_with_message(
        "Successfully marked chat as read!",
        json!(null),
    )))
}

/// POST /api/v1/chat/:guid/unread [Private API required]
pub async fn mark_unread(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);
    let api = state.require_private_api()?;
    let action = actions::mark_chat_unread(&guid);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    // Dispatch webhook event
    if let Some(ref ws) = state.webhook_service {
        ws.dispatch(
            imessage_core::events::CHAT_READ_STATUS_CHANGED,
            json!({ "chatGuid": guid, "read": false }),
            None,
        )
        .await;
    }

    Ok(Json(success_response_with_message(
        "Successfully marked chat as unread!",
        json!(null),
    )))
}

/// POST /api/v1/chat/:guid/leave [Private API required]
pub async fn leave(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);
    let api = state.require_private_api()?;
    let action = actions::leave_chat(&guid);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    Ok(Json(success_response_with_message(
        "Successfully left chat!",
        json!(null),
    )))
}

/// POST /api/v1/chat/:guid/typing [Private API required]
pub async fn start_typing(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);
    let api = state.require_private_api()?;
    let action = actions::start_typing(&guid);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    state.typing_cache.lock().insert(guid);

    Ok(Json(success_response_with_message(
        "Started typing indicator!",
        json!(null),
    )))
}

/// DELETE /api/v1/chat/:guid/typing [Private API required]
pub async fn stop_typing(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);
    let api = state.require_private_api()?;
    let action = actions::stop_typing(&guid);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    state.typing_cache.lock().remove(&guid);

    Ok(Json(success_response_with_message(
        "Stopped typing indicator!",
        json!(null),
    )))
}

/// POST /api/v1/chat/:guid/participant body
#[derive(Debug, Deserialize)]
pub struct ParticipantBody {
    pub address: String,
}

/// Shared logic for add/remove participant.
/// Verifies group chat, sends action, polls DB until participant count changes,
/// returns the updated serialized chat.
async fn toggle_participant(
    state: &AppState,
    guid: &str,
    address: &str,
    action_name: &str,
) -> Result<Json<Value>, AppError> {
    // Verify chat exists and is a group chat
    let (chats, _) = state
        .imessage_repo
        .lock()
        .get_chats(&ChatQueryParams {
            chat_guid: Some(guid.to_string()),
            with_participants: true,
            with_archived: true,
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let chat = chats
        .first()
        .ok_or_else(|| AppError::not_found("Chat does not exist!"))?;

    let prev_count = chat.participants.len();
    if prev_count <= 1 {
        return Err(AppError::imessage_error("Chat is not a group chat!"));
    }

    let api = state.require_private_api()?;
    let action = if action_name == "add" {
        actions::add_participant(guid, address)
    } else {
        actions::remove_participant(guid, address)
    };
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    // Poll DB until participant count changes (30s max)
    let guid_clone = guid.to_string();
    let repo = state.imessage_repo.clone();
    let updated_chat = awaiter::await_condition(
        Duration::from_secs(30),
        || {
            let repo = repo.clone();
            let g = guid_clone.clone();
            async move {
                repo.lock()
                    .get_chats(&ChatQueryParams {
                        chat_guid: Some(g),
                        with_participants: true,
                        with_archived: true,
                        ..Default::default()
                    })
                    .ok()
                    .and_then(|(chats, _)| chats.into_iter().next())
            }
        },
        |c| c.participants.len() != prev_count,
    )
    .await;

    let chat = updated_chat.ok_or_else(|| {
        AppError::imessage_error(&format!(
            "Failed to {action_name} participant to chat! Operation took longer than 30 seconds!"
        ))
    })?;

    let config = ChatSerializerConfig {
        include_participants: true,
        include_messages: false,
    };
    let data = serialize_chat(&chat, &config, false);

    Ok(Json(success_response(data)))
}

/// POST /api/v1/chat/:guid/participant/add [Private API required]
pub async fn add_participant(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    AppJson(body): AppJson<ParticipantBody>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);
    if body.address.is_empty() {
        return Err(AppError::bad_request("address is required"));
    }
    let region = imessage_apple::process::get_region()
        .await
        .unwrap_or_else(|_| "US".to_string());
    let normalized_address = imessage_core::phone::normalize_address(&body.address, &region);
    toggle_participant(&state, &guid, &normalized_address, "add").await
}

/// POST /api/v1/chat/:guid/participant/remove [Private API required]
pub async fn remove_participant(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    AppJson(body): AppJson<ParticipantBody>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);
    if body.address.is_empty() {
        return Err(AppError::bad_request("address is required"));
    }
    let region = imessage_apple::process::get_region()
        .await
        .unwrap_or_else(|_| "US".to_string());
    let normalized_address = imessage_core::phone::normalize_address(&body.address, &region);
    toggle_participant(&state, &guid, &normalized_address, "remove").await
}

/// DELETE /api/v1/chat/:guid/participant [Private API required]
pub async fn remove_participant_delete(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    AppJson(body): AppJson<ParticipantBody>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);
    if body.address.is_empty() {
        return Err(AppError::bad_request("address is required"));
    }
    let region = imessage_apple::process::get_region()
        .await
        .unwrap_or_else(|_| "US".to_string());
    let normalized_address = imessage_core::phone::normalize_address(&body.address, &region);
    toggle_participant(&state, &guid, &normalized_address, "remove").await
}

/// POST /api/v1/chat/:guid/icon [Private API required]
pub async fn set_icon(
    State(state): State<AppState>,
    Path(guid): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);

    // Verify chat is a group chat
    let (chats, _) = state
        .imessage_repo
        .lock()
        .get_chats(&ChatQueryParams {
            chat_guid: Some(guid.clone()),
            with_participants: true,
            with_archived: true,
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let chat = chats
        .first()
        .ok_or_else(|| AppError::not_found("Chat does not exist!"))?;

    if chat.participants.len() <= 1 {
        return Err(AppError::imessage_error("Chat is not a group chat!"));
    }

    let api = state.require_private_api()?;

    // Extract icon file from multipart
    let mut file_data: Option<Vec<u8>> = None;
    let mut file_name = "icon.png".to_string();

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name == "icon" || name == "file" {
            if let Some(fname) = field.file_name() {
                file_name = fname.to_string();
            }
            file_data = Some(
                field
                    .bytes()
                    .await
                    .map_err(|e| AppError::bad_request(&e.to_string()))?
                    .to_vec(),
            );
        } else {
            let _ = field.bytes().await;
        }
    }

    let data = file_data.ok_or_else(|| AppError::bad_request("icon file is required"))?;
    let file_name = sanitize_filename(&file_name, "icon.png");

    // Save to Messages/Attachments (dylib needs files inside Messages sandbox)
    let uuid_str = uuid::Uuid::new_v4().to_string();
    let icon_dir = AppPaths::messages_attachments_dir().join(&uuid_str);
    std::fs::create_dir_all(&icon_dir)
        .map_err(|e| AppError::server_error(&format!("Failed to create directory: {e}")))?;
    let icon_path = icon_dir.join(&file_name);
    std::fs::write(&icon_path, &data)
        .map_err(|e| AppError::server_error(&format!("Failed to write icon: {e}")))?;

    let action = actions::set_group_chat_icon(&guid, Some(&icon_path.to_string_lossy()));
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    Ok(Json(success_response_with_message(
        "Successfully set group chat icon!",
        json!(null),
    )))
}

/// DELETE /api/v1/chat/:guid/icon [Private API required]
pub async fn remove_icon(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);

    // Verify chat is a group chat
    let (chats, _) = state
        .imessage_repo
        .lock()
        .get_chats(&ChatQueryParams {
            chat_guid: Some(guid.clone()),
            with_participants: true,
            with_archived: true,
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let chat = chats
        .first()
        .ok_or_else(|| AppError::not_found("Chat does not exist!"))?;

    if chat.participants.len() <= 1 {
        return Err(AppError::imessage_error("Chat is not a group chat!"));
    }

    let api = state.require_private_api()?;
    // Remove icon by sending null filePath (dylib checks filePath == nil)
    let action = actions::set_group_chat_icon(&guid, None);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    Ok(Json(success_response_with_message(
        "Successfully removed group chat icon!",
        json!(null),
    )))
}

/// POST /api/v1/chat/:guid/share/contact [Private API required]
pub async fn share_contact(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);

    // Verify chat exists
    {
        let (chats, _) = state
            .imessage_repo
            .lock()
            .get_chats(&ChatQueryParams {
                chat_guid: Some(guid.clone()),
                with_archived: true,
                ..Default::default()
            })
            .map_err(|e| AppError::server_error(&e.to_string()))?;

        if chats.is_empty() {
            return Err(AppError::not_found("Chat does not exist!"));
        }
    }

    let api = state.require_private_api()?;
    let action = actions::share_contact_card(&guid);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    Ok(Json(success_response_with_message(
        "Successfully shared contact card!",
        json!(null),
    )))
}

/// DELETE /api/v1/chat/:guid/:messageGuid [Private API required]
pub async fn delete_message(
    State(state): State<AppState>,
    Path((guid, message_guid)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);

    // Verify chat exists
    {
        let (chats, _) = state
            .imessage_repo
            .lock()
            .get_chats(&ChatQueryParams {
                chat_guid: Some(guid.clone()),
                with_archived: true,
                ..Default::default()
            })
            .map_err(|e| AppError::server_error(&e.to_string()))?;

        if chats.is_empty() {
            return Err(AppError::not_found("Chat does not exist!"));
        }
    }

    // Verify message exists
    {
        let msg = state
            .imessage_repo
            .lock()
            .get_message(&message_guid, false, false)
            .map_err(|e| AppError::server_error(&e.to_string()))?;

        if msg.is_none() {
            return Err(AppError::not_found("Message does not exist!"));
        }
    }

    let api = state.require_private_api()?;
    let action = actions::delete_message(&guid, &message_guid);
    api.send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    Ok(Json(success_response_with_message(
        "Successfully deleted message!",
        json!(null),
    )))
}

/// GET /api/v1/chat/:guid/share/contact/status [Private API]
pub async fn share_contact_status(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<Json<Value>, AppError> {
    let guid = normalize_chat_guid(&guid);

    // Verify chat exists
    {
        let (chats, _) = state
            .imessage_repo
            .lock()
            .get_chats(&ChatQueryParams {
                chat_guid: Some(guid.clone()),
                with_archived: true,
                ..Default::default()
            })
            .map_err(|e| AppError::server_error(&e.to_string()))?;

        if chats.is_empty() {
            return Err(AppError::not_found("Chat does not exist!"));
        }
    }

    let api = state.require_private_api()?;
    let action = actions::should_offer_contact_sharing(&guid);
    let result = api
        .send_action(action)
        .await
        .map_err(|e| AppError::imessage_error(&e.to_string()))?;

    let can_share = result
        .and_then(|txn| txn.data)
        .and_then(|d| d.get("share").and_then(|v| v.as_bool()))
        .ok_or_else(|| {
            AppError::server_error("Failed to check if contact sharing is available!")
        })?;

    Ok(Json(success_response_with_message(
        "Successfully got contact sharing status!",
        json!(can_share),
    )))
}

/// GET /api/v1/chat/:guid/icon
pub async fn get_icon(
    State(state): State<AppState>,
    Path(guid): Path<String>,
) -> Result<([(axum::http::HeaderName, String); 3], axum::body::Body), AppError> {
    use axum::body::Body;
    use axum::http::header;
    use tokio::fs::File;
    use tokio_util::io::ReaderStream;

    let guid = normalize_chat_guid(&guid);

    let (chats, _) = state
        .imessage_repo
        .lock()
        .get_chats(&ChatQueryParams {
            chat_guid: Some(guid.clone()),
            with_archived: true,
            ..Default::default()
        })
        .map_err(|e| AppError::server_error(&e.to_string()))?;

    let chat = chats
        .first()
        .ok_or_else(|| AppError::not_found("Chat does not exist!"))?;

    // Extract groupPhotoGuid from chat.properties blob by scanning raw bytes
    let icon_guid = chat
        .properties
        .as_ref()
        .and_then(|blob| extract_group_photo_guid(blob))
        .ok_or_else(|| AppError::not_found("Unable to find icon for the selected chat"))?;

    let attachment = state
        .imessage_repo
        .lock()
        .get_attachment(&icon_guid)
        .map_err(|e| AppError::server_error(&e.to_string()))?
        .ok_or_else(|| AppError::not_found("Unable to find icon for the selected chat"))?;

    let file_path = attachment
        .filename
        .as_deref()
        .ok_or_else(|| AppError::not_found("Icon file path not found"))?;

    let real_path = imessage_core::utils::expand_tilde(file_path);
    if !real_path.exists() {
        return Err(AppError::not_found(
            "Unable to find icon for the selected chat",
        ));
    }

    let file = File::open(&real_path)
        .await
        .map_err(|e| AppError::server_error(&format!("Failed to open file: {e}")))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|e| AppError::server_error(&format!("Failed to read metadata: {e}")))?;

    let mime_type = attachment.mime_type.as_deref().unwrap_or("image/jfif");
    let file_name = sanitize_header_filename(
        real_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("icon"),
        "icon",
    );

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    Ok((
        [
            (header::CONTENT_TYPE, mime_type.to_string()),
            (header::CONTENT_LENGTH, metadata.len().to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file_name}\""),
            ),
        ],
        body,
    ))
}

/// Extract groupPhotoGuid from the chat.properties binary plist blob.
///
/// On Tahoe the blob is a plain binary plist with a top-level `groupPhotoGuid` key
/// containing a UUID string. On older macOS it may be an `at_0_<UUID>` style GUID
/// embedded in the blob bytes. We try plist decoding first, then fall back to byte
/// scanning for `at_` patterns.
fn extract_group_photo_guid(blob: &[u8]) -> Option<String> {
    // Try proper plist decoding first (works on Tahoe and modern macOS)
    if let Ok(value) = plist::Value::from_reader(std::io::Cursor::new(blob))
        && let Some(dict) = value.as_dictionary()
        && let Some(guid) = dict.get("groupPhotoGuid").and_then(|v| v.as_string())
        && !guid.is_empty()
    {
        return Some(guid.to_string());
    }

    // Fallback: scan raw bytes for "at_" prefixed attachment GUIDs (older macOS)
    let mut last_guid: Option<String> = None;
    for i in 0..blob.len().saturating_sub(3) {
        if blob[i..].starts_with(b"at_") {
            let end = blob[i..]
                .iter()
                .position(|&b| b == 0 || b < 0x20)
                .unwrap_or(blob.len() - i);
            if let Ok(s) = std::str::from_utf8(&blob[i..i + end])
                && s.len() > 10
                && s.starts_with("at_")
            {
                let guid_part = s.split('/').next().unwrap_or(s);
                last_guid = Some(guid_part.to_string());
            }
        }
    }

    last_guid
}
