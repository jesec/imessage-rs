/// HTTP server setup: builds the Axum router with all routes, middleware, and CORS.
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::middleware as axum_mw;
use axum::routing::{delete, get, post};
use tower_http::cors::CorsLayer;
use tower_http::timeout::TimeoutLayer;
use tracing::info;

use crate::middleware::auth::auth_middleware;
use crate::middleware::pretty::pretty_json_middleware;
use crate::routes::{
    attachment, chat, facetime, general, handle, icloud, message, server, webhook,
};
use crate::state::AppState;

/// Build the full Axum router.
pub fn build_router(state: AppState) -> Router {
    // Routes that require authentication
    let protected = Router::new()
        // General
        .route("/api/v1/ping", get(general::ping))
        // Server
        .route("/api/v1/server/info", get(server::get_info))
        .route("/api/v1/server/logs", get(server::get_logs))
        .route("/api/v1/server/permissions", get(server::get_permissions))
        .route(
            "/api/v1/server/statistics/totals",
            get(server::get_stat_totals),
        )
        .route(
            "/api/v1/server/statistics/media",
            get(server::get_stat_media),
        )
        .route(
            "/api/v1/server/statistics/media/chat",
            get(server::get_stat_media_by_chat),
        )
        // Webhook
        .route("/api/v1/webhook", get(webhook::get_webhooks))
        // Handle
        .route("/api/v1/handle/count", get(handle::count))
        .route("/api/v1/handle/query", post(handle::query))
        .route("/api/v1/handle/{guid}", get(handle::find))
        .route("/api/v1/handle/{guid}/focus", get(handle::get_focus_status))
        .route(
            "/api/v1/handle/availability/imessage",
            get(handle::get_imessage_availability).post(handle::post_imessage_availability),
        )
        .route(
            "/api/v1/handle/availability/facetime",
            get(handle::get_facetime_availability).post(handle::post_facetime_availability),
        )
        // Chat — NOTE: share/contact/status MUST be before the {guid}/{messageGuid} catch-all
        .route("/api/v1/chat/count", get(chat::count))
        .route("/api/v1/chat/query", post(chat::query))
        .route("/api/v1/chat/new", post(chat::create))
        .route("/api/v1/chat/{guid}/message", get(chat::get_messages))
        .route(
            "/api/v1/chat/{guid}",
            get(chat::find).put(chat::update).delete(chat::delete_chat),
        )
        .route("/api/v1/chat/{guid}/read", post(chat::mark_read))
        .route("/api/v1/chat/{guid}/unread", post(chat::mark_unread))
        .route("/api/v1/chat/{guid}/leave", post(chat::leave))
        .route(
            "/api/v1/chat/{guid}/typing",
            post(chat::start_typing).delete(chat::stop_typing),
        )
        .route(
            "/api/v1/chat/{guid}/participant/add",
            post(chat::add_participant),
        )
        .route(
            "/api/v1/chat/{guid}/participant/remove",
            post(chat::remove_participant),
        )
        .route(
            "/api/v1/chat/{guid}/participant",
            post(chat::add_participant).delete(chat::remove_participant_delete),
        )
        .route(
            "/api/v1/chat/{guid}/icon",
            get(chat::get_icon)
                .post(chat::set_icon)
                .delete(chat::remove_icon),
        )
        .route(
            "/api/v1/chat/{guid}/share/contact/status",
            get(chat::share_contact_status),
        )
        .route(
            "/api/v1/chat/{guid}/share/contact",
            post(chat::share_contact),
        )
        .route(
            "/api/v1/chat/{guid}/{messageGuid}",
            delete(chat::delete_message),
        )
        // Message
        .route("/api/v1/message/count", get(message::count))
        .route("/api/v1/message/count/updated", get(message::count_updated))
        .route("/api/v1/message/count/me", get(message::sent_count))
        .route("/api/v1/message/query", post(message::query))
        .route("/api/v1/message/text", post(message::send_text))
        .route("/api/v1/message/attachment", post(message::send_attachment))
        .route(
            "/api/v1/message/attachment/chunk",
            post(message::send_attachment_chunk),
        )
        .route("/api/v1/message/multipart", post(message::send_multipart))
        .route("/api/v1/message/react", post(message::send_reaction))
        .route("/api/v1/message/{guid}/edit", post(message::edit_message))
        .route(
            "/api/v1/message/{guid}/unsend",
            post(message::unsend_message),
        )
        .route(
            "/api/v1/message/{guid}/notify",
            post(message::notify_message),
        )
        .route(
            "/api/v1/message/{guid}/embedded-media",
            get(message::get_embedded_media),
        )
        .route("/api/v1/message/{guid}", get(message::find))
        // Attachment
        .route("/api/v1/attachment/count", get(attachment::count))
        .route("/api/v1/attachment/upload", post(attachment::upload))
        .route(
            "/api/v1/attachment/{guid}/download/force",
            get(attachment::force_download),
        )
        .route(
            "/api/v1/attachment/{guid}/download",
            get(attachment::download),
        )
        .route(
            "/api/v1/attachment/{guid}/live",
            get(attachment::download_live),
        )
        .route(
            "/api/v1/attachment/{guid}/blurhash",
            get(attachment::blurhash),
        )
        .route("/api/v1/attachment/{guid}", get(attachment::find))
        // iCloud
        .route("/api/v1/icloud/account", get(icloud::get_account_info))
        .route("/api/v1/icloud/account/alias", post(icloud::change_alias))
        .route("/api/v1/icloud/contact", get(icloud::get_contact_card))
        .route(
            "/api/v1/icloud/findmy/devices",
            get(icloud::get_findmy_devices),
        )
        .route(
            "/api/v1/icloud/findmy/devices/refresh",
            post(icloud::refresh_findmy_devices),
        )
        .route(
            "/api/v1/icloud/findmy/friends",
            get(icloud::get_findmy_friends),
        )
        .route(
            "/api/v1/icloud/findmy/friends/refresh",
            post(icloud::refresh_findmy_friends),
        )
        // FaceTime
        .route("/api/v1/facetime/session", post(facetime::create_session))
        .route(
            "/api/v1/facetime/answer/{call_uuid}",
            post(facetime::answer_call),
        )
        .route(
            "/api/v1/facetime/leave/{call_uuid}",
            post(facetime::leave_call),
        )
        // Auth middleware applies to all routes
        .layer(axum_mw::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state.clone());

    // Compose the full app
    Router::new()
        .merge(protected)
        // ?pretty JSON middleware (must be before CORS layer)
        .layer(axum_mw::from_fn(pretty_json_middleware))
        // CORS (permissive)
        .layer(CorsLayer::permissive())
        // Request timeout: 5 minutes
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(300),
        ))
        // Body size limit: 1GB
        .layer(DefaultBodyLimit::max(1024 * 1024 * 1024))
        // Fallback: plain text "Not Found" for unknown routes
        .fallback(fallback_handler)
}

/// 404 handler: plain text "Not Found" (not JSON).
async fn fallback_handler() -> (axum::http::StatusCode, &'static str) {
    (axum::http::StatusCode::NOT_FOUND, "Not Found")
}

/// Start the HTTP server.
pub async fn start_server(state: AppState) -> anyhow::Result<()> {
    let port = state.config.socket_port;
    let app = build_router(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Successfully started HTTP server on {addr}");
    axum::serve(listener, app).await?;

    Ok(())
}
