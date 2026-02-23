mod config;
mod process;

use anyhow::Result;
use clap::Parser;
use config::{CliArgs, Command, bootstrap_config, resolve_config};
use imessage_core::config::{AppConfig, AppPaths, WebhookConfigEntry, setup_directories};
use imessage_core::macos::{macos_version, require_min_sequoia};
use imessage_db::imessage::repository::MessageRepository;
use imessage_http::server::start_server;
use imessage_http::state::AppState;
use imessage_private_api::events::{
    FaceTimeStatus, event_types, parse_facetime_event, parse_findmy_locations, parse_typing_event,
};
use imessage_private_api::injection::inject_app_dylib;
use imessage_private_api::service::PrivateApiService;
use imessage_watcher::listener::IMessageListener;
use imessage_webhooks::service::WebhookService;
use parking_lot::Mutex;
use std::fs;
use std::sync::Arc;
use tracing::{error, info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn main() -> Result<()> {
    // Initialize logging: stdout + file (~/Library/Application Support/imessage-rs/logs/main.log)
    let log_dir = AppPaths::user_data().join("logs");
    let _ = fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::never(&log_dir, "main.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_thread_ids(false),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_thread_ids(false)
                .with_ansi(false)
                .with_writer(non_blocking),
        )
        .init();

    // Parse CLI args
    let cli = CliArgs::parse();

    // Handle bootstrap subcommand (write config and exit)
    if let Some(Command::Bootstrap { flags }) = cli.command {
        return bootstrap_config(flags);
    }

    // Resolve config: --config path | CLI flags only | default config.yml
    let config = resolve_config(&cli)?;

    // Single-instance check via PID file
    let pid_file = AppPaths::pid_file();
    if let Some(parent) = pid_file.parent() {
        fs::create_dir_all(parent)?;
    }

    if pid_file.exists()
        && let Ok(contents) = fs::read_to_string(&pid_file)
        && let Ok(existing_pid) = contents.trim().parse::<i32>()
        && process::is_running(existing_pid)
    {
        error!("imessage-rs is already running (PID {existing_pid})! Exiting...");
        std::process::exit(1);
    }

    // Write our PID
    fs::write(&pid_file, std::process::id().to_string())?;

    // Detect macOS version and enforce minimum
    let os_version = macos_version();
    if let Err(msg) = require_min_sequoia() {
        error!("{msg}");
        std::process::exit(1);
    }

    // Setup filesystem directories
    if let Err(e) = setup_directories() {
        error!("Failed to setup filesystem: {e}");
    }

    // Log startup metadata
    info!("Starting imessage-rs v{}", AppPaths::version());
    info!("macOS version: {os_version}");
    info!("HTTP port: {}", config.socket_port);
    info!(
        "Private API: Messages={}, FaceTime={}, FindMy={}",
        if config.enable_private_api {
            "enabled"
        } else {
            "disabled"
        },
        if config.enable_facetime_private_api {
            "enabled"
        } else {
            "disabled"
        },
        if config.enable_findmy_private_api {
            "enabled"
        } else {
            "disabled"
        },
    );

    if config.password.is_empty() {
        warn!("No password is currently set! imessage-rs will not function correctly without one.");
    }

    // Start the async runtime
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move { run_server(config).await })?;

    // Cleanup PID file
    let _ = fs::remove_file(&pid_file);

    Ok(())
}

async fn run_server(config: AppConfig) -> Result<()> {
    // Open the iMessage database
    let db_path = AppPaths::imessage_db();
    info!("Opening iMessage database at {}", db_path.display());
    let repo = MessageRepository::open(db_path.clone())?;
    info!(
        "Connected to chat.db ({} messages)",
        repo.get_message_count(&Default::default()).unwrap_or(0)
    );

    // Initialize webhook service and load targets from config
    let webhook_service = Arc::new(WebhookService::new(&config));
    {
        use imessage_webhooks::WebhookTarget;
        let targets: Vec<WebhookTarget> = config
            .webhooks
            .iter()
            .map(|wh| match wh {
                WebhookConfigEntry::Simple(url) => WebhookTarget {
                    url: url.clone(),
                    events: vec!["*".to_string()],
                },
                WebhookConfigEntry::Detailed { url, events } => WebhookTarget {
                    url: url.clone(),
                    events: events.clone().unwrap_or_else(|| vec!["*".to_string()]),
                },
            })
            .collect();
        info!("Loaded {} webhook targets", targets.len());
        webhook_service.set_targets(targets).await;
    }

    // Start the iMessage file watcher
    let watcher_repo = MessageRepository::open(db_path.clone())?;
    let watcher_repo = Arc::new(Mutex::new(watcher_repo));
    let (listener, mut watcher_rx) = IMessageListener::new(db_path);

    let webhook_for_watcher = webhook_service.clone();
    let watcher_handle = tokio::spawn(async move {
        if let Err(e) = listener.start(watcher_repo).await {
            error!("iMessage listener error: {e}");
        }
    });

    // Bridge watcher events → webhook dispatch
    let webhook_bridge_handle = tokio::spawn(async move {
        loop {
            match watcher_rx.recv().await {
                Ok(event) => {
                    let dedup_key = event
                        .data
                        .get("guid")
                        .and_then(|v| v.as_str())
                        .map(|g| format!("{}-{}", event.event_type, g));

                    webhook_for_watcher
                        .dispatch(&event.event_type, event.data, dedup_key.as_deref())
                        .await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Webhook bridge lagged by {n} events");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    info!("Watcher channel closed, stopping webhook bridge");
                    break;
                }
            }
        }
    });

    // Start Private API TCP service (if any Private API feature is enabled)
    let need_private_api = config.enable_private_api
        || config.enable_facetime_private_api
        || config.enable_findmy_private_api;
    let (private_api_service, private_api_handle) = if need_private_api {
        let mut private_api = PrivateApiService::new();
        match private_api.start().await {
            Ok(handle) => {
                info!("Private API service started");
                let service = Arc::new(private_api);

                // Inject dylib into Messages.app (iMessage Private API)
                if config.enable_private_api {
                    let svc = service.clone();
                    tokio::spawn(async move {
                        inject_app_dylib(&svc, "Messages").await;
                    });
                }

                // Inject dylib into FaceTime.app (FaceTime Private API)
                if config.enable_facetime_private_api {
                    let svc = service.clone();
                    tokio::spawn(async move {
                        inject_app_dylib(&svc, "FaceTime").await;
                    });
                }

                // Inject dylib into FindMy.app (FindMy device cache decryption)
                if config.enable_findmy_private_api {
                    let svc = service.clone();
                    tokio::spawn(async move {
                        inject_app_dylib(&svc, "FindMy").await;
                    });
                }

                (Some(service), Some(handle))
            }
            Err(e) => {
                warn!("Failed to start Private API service: {e}");
                (None, None)
            }
        }
    } else {
        info!("Private API is disabled, skipping TCP service");
        (None, None)
    };

    // Keep config values for cleanup
    let cleanup_enable_private_api = config.enable_private_api;
    let cleanup_enable_facetime = config.enable_facetime_private_api;
    let cleanup_enable_findmy = config.enable_findmy_private_api;

    // Build shared state
    let state = AppState::new(
        config,
        repo,
        private_api_service,
        Some(webhook_service.clone()),
    );

    // Spawn FindMy event handler: subscribe to Private API events and populate
    // the findmy_friends_cache when new-findmy-location events arrive
    if let Some(ref api) = state.private_api {
        let mut event_rx = api.subscribe_events();
        let cache = state.findmy_friends_cache.clone();
        tokio::spawn(async move {
            loop {
                match event_rx.recv().await {
                    Ok(raw) => {
                        if raw.event.as_deref() == Some(event_types::NEW_FINDMY_LOCATION) {
                            let locations = parse_findmy_locations(&raw);
                            let mut cache = cache.lock();
                            for loc in locations {
                                let new_entry = serde_json::json!({
                                    "handle": loc.handle,
                                    "coordinates": [loc.coordinates.0, loc.coordinates.1],
                                    "long_address": loc.long_address,
                                    "short_address": loc.short_address,
                                    "subtitle": loc.subtitle,
                                    "title": loc.title,
                                    "last_updated": loc.last_updated,
                                    "is_locating_in_progress": loc.is_locating_in_progress,
                                    "status": loc.status,
                                });

                                // Deduplication: reject stale/bad updates
                                if let Some(existing) = cache.0.get(&loc.handle) {
                                    let existing_ts = existing
                                        .get("last_updated")
                                        .and_then(|v| v.as_i64())
                                        .unwrap_or(0);
                                    let new_ts = loc.last_updated.unwrap_or(0);

                                    // Reject older timestamps
                                    if new_ts < existing_ts {
                                        continue;
                                    }
                                    // Reject zero-coordinate updates over real coordinates
                                    if loc.coordinates == (0.0, 0.0) {
                                        let ex_coords =
                                            existing.get("coordinates").and_then(|c| c.as_array());
                                        if let Some(c) = ex_coords
                                            && c.len() == 2
                                        {
                                            let ex_lat = c[0].as_f64().unwrap_or(0.0);
                                            let ex_lon = c[1].as_f64().unwrap_or(0.0);
                                            if ex_lat != 0.0 || ex_lon != 0.0 {
                                                continue;
                                            }
                                        }
                                    }
                                }

                                cache.0.insert(loc.handle.clone(), new_entry);
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("FindMy event handler lagged by {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
        });
    }

    // Bridge Private API events → webhook dispatch (typing, FaceTime, aliases, FindMy)
    let pa_webhook_handle = if let Some(ref api) = state.private_api {
        let mut event_rx = api.subscribe_events();
        let ws = webhook_service.clone();
        Some(tokio::spawn(async move {
            loop {
                match event_rx.recv().await {
                    Ok(raw) => {
                        let event_type = match raw.event.as_deref() {
                            Some(e) => e,
                            None => continue,
                        };
                        match event_type {
                            event_types::STARTED_TYPING
                            | event_types::TYPING
                            | event_types::STOPPED_TYPING => {
                                if let Some(te) = parse_typing_event(&raw) {
                                    ws.dispatch(
                                        imessage_core::events::TYPING_INDICATOR,
                                        serde_json::json!({
                                            "guid": te.guid,
                                            "display": te.is_typing,
                                        }),
                                        None,
                                    )
                                    .await;
                                }
                            }
                            event_types::FACETIME_CALL_STATUS_CHANGED => {
                                if let Some(facetime_event) = parse_facetime_event(&raw) {
                                    let facetime_data = serde_json::json!({
                                        "callUuid": facetime_event.call_uuid,
                                        "status": facetime_event.status.as_str(),
                                        "statusId": facetime_event.status_id,
                                        "address": facetime_event.address,
                                        "endedError": facetime_event.ended_error,
                                        "endedReason": facetime_event.ended_reason,
                                        "imageUrl": facetime_event.image_url,
                                        "isOutgoing": facetime_event.is_outgoing,
                                        "isAudio": facetime_event.is_audio,
                                        "isVideo": facetime_event.is_video,
                                    });
                                    ws.dispatch(
                                        imessage_core::events::FACETIME_CALL_STATUS_CHANGED,
                                        facetime_data.clone(),
                                        None,
                                    )
                                    .await;
                                    if facetime_event.status == FaceTimeStatus::Incoming {
                                        ws.dispatch(
                                            imessage_core::events::INCOMING_FACETIME,
                                            facetime_data,
                                            None,
                                        )
                                        .await;
                                    }
                                }
                            }
                            event_types::ALIASES_REMOVED => {
                                if let Some(data) = raw.data.clone() {
                                    ws.dispatch(
                                        imessage_core::events::IMESSAGE_ALIASES_REMOVED,
                                        data,
                                        None,
                                    )
                                    .await;
                                }
                            }
                            event_types::NEW_FINDMY_LOCATION => {
                                let locations = parse_findmy_locations(&raw);
                                if !locations.is_empty() {
                                    let loc_json: Vec<serde_json::Value> = locations
                                        .iter()
                                        .map(|loc| {
                                            serde_json::json!({
                                                "handle": loc.handle,
                                                "coordinates": [loc.coordinates.0, loc.coordinates.1],
                                                "long_address": loc.long_address,
                                                "short_address": loc.short_address,
                                                "subtitle": loc.subtitle,
                                                "title": loc.title,
                                                "last_updated": loc.last_updated,
                                                "is_locating_in_progress": loc.is_locating_in_progress,
                                                "status": loc.status,
                                            })
                                        })
                                        .collect();
                                    ws.dispatch(
                                        imessage_core::events::NEW_FINDMY_LOCATION,
                                        serde_json::json!(loc_json),
                                        None,
                                    )
                                    .await;
                                }
                            }
                            _ => {}
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("PA webhook bridge lagged by {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!("PA event channel closed, stopping webhook bridge");
                        break;
                    }
                }
            }
        }))
    } else {
        None
    };

    // Spawn send cache purge task: every 5 minutes, remove entries older than 5 minutes
    {
        let send_cache = state.send_cache.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                let mut cache = send_cache.lock();
                let cutoff = std::time::Instant::now() - std::time::Duration::from_secs(300);
                cache.retain(|_, ts| *ts > cutoff);
            }
        });
    }

    // Dispatch hello-world event after a short delay (allows webhooks to be ready)
    {
        let ws = webhook_service.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            ws.dispatch(
                imessage_core::events::HELLO_WORLD,
                serde_json::json!({
                    "server_address": ws.server_address(),
                    "version": imessage_core::config::AppPaths::version(),
                }),
                None,
            )
            .await;
        });
    }

    // Start HTTP server (blocks until shutdown)
    tokio::select! {
        result = start_server(state) => {
            if let Err(e) = result {
                error!("HTTP server error: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received shutdown signal, stopping...");
        }
    }

    // Cleanup
    watcher_handle.abort();
    webhook_bridge_handle.abort();
    if let Some(h) = pa_webhook_handle {
        h.abort();
    }
    if let Some(h) = private_api_handle {
        h.abort();
    }

    // Kill injected app processes on clean shutdown
    if cleanup_enable_private_api {
        info!("Stopping injected Messages.app process...");
        let _ = tokio::process::Command::new("killall")
            .arg("Messages")
            .output()
            .await;
    }
    if cleanup_enable_findmy {
        info!("Stopping injected FindMy.app process...");
        let _ = tokio::process::Command::new("killall")
            .arg("FindMy")
            .output()
            .await;
    }
    if cleanup_enable_facetime {
        info!("Stopping injected FaceTime.app process...");
        let _ = tokio::process::Command::new("killall")
            .arg("FaceTime")
            .output()
            .await;
    }

    let pid_file = AppPaths::pid_file();
    let _ = fs::remove_file(&pid_file);

    info!("Server stopped.");
    Ok(())
}
