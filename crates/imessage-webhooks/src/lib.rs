/// Webhook dispatch service: fire-and-forget HTTP POST to registered webhooks.
///
/// When events occur (new message, typing, etc.), the webhook service POSTs
/// a JSON payload to each registered webhook URL whose event filter matches.
pub mod event_cache;
pub mod service;

/// A webhook target: URL + event filter.
#[derive(Debug, Clone)]
pub struct WebhookTarget {
    pub url: String,
    /// Event names this webhook subscribes to (e.g. `["*"]` or `["new-message"]`).
    pub events: Vec<String>,
}
