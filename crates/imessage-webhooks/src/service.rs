/// Webhook dispatch service.
///
/// Fire-and-forget HTTP POST to all registered webhooks whose event filter matches.
/// Webhooks with event filter ["*"] match all events.
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::{info, warn};

use imessage_core::config::AppConfig;

use crate::WebhookTarget;
use crate::event_cache::EventCache;

/// The webhook dispatch service.
pub struct WebhookService {
    client: reqwest::Client,
    targets: Arc<Mutex<Vec<WebhookTarget>>>,
    event_cache: Arc<Mutex<EventCache>>,
    server_address: String,
}

impl WebhookService {
    pub fn new(config: &AppConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self {
            client,
            targets: Arc::new(Mutex::new(Vec::new())),
            event_cache: Arc::new(Mutex::new(EventCache::new())),
            server_address: config.server_address.clone(),
        }
    }

    /// Get the configured server address.
    pub fn server_address(&self) -> &str {
        &self.server_address
    }

    /// Set the list of webhook targets (called on startup).
    pub async fn set_targets(&self, targets: Vec<WebhookTarget>) {
        let mut t = self.targets.lock().await;
        *t = targets;
    }

    /// Get a snapshot of the current webhook targets.
    pub async fn get_targets(&self) -> Vec<WebhookTarget> {
        self.targets.lock().await.clone()
    }

    /// Dispatch an event to all matching webhooks.
    ///
    /// - `event_type`: e.g., "new-message", "typing-indicator"
    /// - `data`: the event payload
    /// - `dedup_key`: optional key for event deduplication
    pub async fn dispatch(&self, event_type: &str, data: Value, dedup_key: Option<&str>) {
        // Check dedup
        if let Some(key) = dedup_key {
            let mut cache = self.event_cache.lock().await;
            if cache.is_duplicate(key) {
                return;
            }
        }

        let targets = self.targets.lock().await.clone();
        if targets.is_empty() {
            return;
        }

        let payload = json!({
            "type": event_type,
            "data": data,
        });

        for target in &targets {
            if !Self::matches_event(target, event_type) {
                continue;
            }

            let client = self.client.clone();
            let url = target.url.clone();
            let payload = payload.clone();

            // Fire and forget
            tokio::spawn(async move {
                match client.post(&url).json(&payload).send().await {
                    Ok(resp) => {
                        if !resp.status().is_success() {
                            warn!("Webhook {url} returned status {}", resp.status());
                        }
                    }
                    Err(e) => {
                        warn!("Webhook {url} failed: {e}");
                    }
                }
            });
        }

        info!(
            "Dispatched '{event_type}' to {} matching webhooks",
            targets
                .iter()
                .filter(|t| Self::matches_event(t, event_type))
                .count()
        );
    }

    /// Check if a webhook's event filter matches the given event type.
    fn matches_event(target: &WebhookTarget, event_type: &str) -> bool {
        target.events.iter().any(|e| e == "*" || e == event_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_webhook(events: Vec<String>) -> WebhookTarget {
        WebhookTarget {
            url: "http://example.com".to_string(),
            events,
        }
    }

    #[test]
    fn wildcard_matches_all() {
        let target = test_webhook(vec!["*".to_string()]);
        assert!(WebhookService::matches_event(&target, "new-message"));
        assert!(WebhookService::matches_event(&target, "typing-indicator"));
        assert!(WebhookService::matches_event(&target, "anything"));
    }

    #[test]
    fn specific_events_filter() {
        let target = test_webhook(vec![
            "new-message".to_string(),
            "updated-message".to_string(),
        ]);
        assert!(WebhookService::matches_event(&target, "new-message"));
        assert!(WebhookService::matches_event(&target, "updated-message"));
        assert!(!WebhookService::matches_event(&target, "typing-indicator"));
    }

    #[test]
    fn empty_events_matches_nothing() {
        let target = test_webhook(vec![]);
        assert!(!WebhookService::matches_event(&target, "new-message"));
    }
}
