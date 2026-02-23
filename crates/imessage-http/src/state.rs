use parking_lot::Mutex;
/// Application state shared across all routes via Axum's State extractor.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;

use imessage_core::config::AppConfig;
use imessage_db::imessage::repository::MessageRepository;
use imessage_private_api::service::PrivateApiService;
use imessage_webhooks::service::WebhookService;

use crate::middleware::error::AppError;

/// Cached FindMy friends locations (handle -> location JSON) + last refresh time.
pub type FindMyFriendsCache = (HashMap<String, Value>, Option<Instant>);

/// Shared application state.
/// MessageRepository wraps rusqlite::Connection which is not Send+Sync,
/// so we wrap it in a Mutex for thread-safe access.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub imessage_repo: Arc<Mutex<MessageRepository>>,
    pub private_api: Option<Arc<PrivateApiService>>,
    pub webhook_service: Option<Arc<WebhookService>>,
    /// Cached FindMy friends locations (handle -> location JSON) + last refresh time
    pub findmy_friends_cache: Arc<Mutex<FindMyFriendsCache>>,
    /// Cached FindMy device decryption key (32 bytes, from Keychain via FindMy.app)
    pub findmy_key: Arc<Mutex<Option<[u8; 32]>>>,
    /// Send cache for deduplication (tempGuid -> timestamp)
    pub send_cache: Arc<Mutex<HashMap<String, Instant>>>,
    /// Typing cache: tracks chats with active typing indicators (auto-stopped before sends)
    pub typing_cache: Arc<Mutex<HashSet<String>>>,
    /// Mutex to prevent concurrent FindMy device refreshes
    pub findmy_refresh_lock: Arc<tokio::sync::Mutex<()>>,
}

impl AppState {
    pub fn new(
        config: AppConfig,
        imessage_repo: MessageRepository,
        private_api: Option<Arc<PrivateApiService>>,
        webhook_service: Option<Arc<WebhookService>>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            imessage_repo: Arc::new(Mutex::new(imessage_repo)),
            private_api,
            webhook_service,
            findmy_friends_cache: Arc::new(Mutex::new((HashMap::new(), None))),
            findmy_key: Arc::new(Mutex::new(None)),
            send_cache: Arc::new(Mutex::new(HashMap::new())),
            typing_cache: Arc::new(Mutex::new(HashSet::new())),
            findmy_refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Get a reference to the Private API service, or return an error if not available/ready.
    /// Requires the Messages.app dylib to have completed IMCore initialization.
    pub fn require_private_api(&self) -> Result<Arc<PrivateApiService>, AppError> {
        let api = self
            .private_api
            .as_ref()
            .ok_or_else(|| AppError::imessage_error("Private API is not enabled"))?;
        if !api.is_messages_ready() {
            return Err(AppError::imessage_error(
                "Private API helper is not connected",
            ));
        }
        Ok(api.clone())
    }

    /// Get a reference to the FindMy Private API service, or return an error if not enabled/ready.
    /// Requires the FindMy.app dylib to be connected and ready.
    pub fn require_findmy_private_api(&self) -> Result<Arc<PrivateApiService>, AppError> {
        if !self.config.enable_findmy_private_api {
            return Err(AppError::imessage_error(
                "FindMy Private API is not enabled",
            ));
        }
        let api = self
            .private_api
            .as_ref()
            .ok_or_else(|| AppError::imessage_error("FindMy Private API is not enabled"))?;
        if !api.is_findmy_ready() {
            return Err(AppError::imessage_error(
                "FindMy Private API helper is not connected",
            ));
        }
        Ok(api.clone())
    }

    /// Get a reference to the FaceTime Private API service, or return an error if not enabled/ready.
    /// Requires the FaceTime.app dylib to be connected and ready.
    pub fn require_facetime_private_api(&self) -> Result<Arc<PrivateApiService>, AppError> {
        if !self.config.enable_facetime_private_api {
            return Err(AppError::imessage_error(
                "FaceTime Private API is not enabled",
            ));
        }
        let api = self
            .private_api
            .as_ref()
            .ok_or_else(|| AppError::imessage_error("FaceTime Private API is not enabled"))?;
        if !api.is_facetime_ready() {
            return Err(AppError::imessage_error(
                "FaceTime Private API helper is not connected",
            ));
        }
        Ok(api.clone())
    }

    /// Check if a tempGuid is already in the send cache
    pub fn is_send_cached(&self, temp_guid: &str) -> bool {
        self.send_cache.lock().contains_key(temp_guid)
    }

    /// Add a tempGuid to the send cache
    pub fn cache_send(&self, temp_guid: String) {
        self.send_cache.lock().insert(temp_guid, Instant::now());
    }

    /// Remove a tempGuid from the send cache
    pub fn uncache_send(&self, temp_guid: &str) {
        self.send_cache.lock().remove(temp_guid);
    }
}
