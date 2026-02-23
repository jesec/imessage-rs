/// Event deduplication cache with 1-hour TTL.
///
/// Prevents duplicate webhook deliveries when the same event is detected
/// multiple times (e.g., during DB polling overlap).
use std::collections::HashMap;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(3600); // 1 hour

pub struct EventCache {
    entries: HashMap<String, Instant>,
}

impl EventCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Check if an event key has been seen recently.
    /// Returns true if the event is a duplicate (should be skipped).
    pub fn is_duplicate(&mut self, key: &str) -> bool {
        self.cleanup();

        if self.entries.contains_key(key) {
            return true;
        }

        self.entries.insert(key.to_string(), Instant::now());
        false
    }

    /// Remove expired entries.
    fn cleanup(&mut self) {
        let now = Instant::now();
        self.entries.retain(|_, ts| now.duration_since(*ts) < TTL);
    }

    /// Number of cached entries (for testing).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for EventCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_events_are_not_duplicates() {
        let mut cache = EventCache::new();
        assert!(!cache.is_duplicate("event-1"));
        assert!(!cache.is_duplicate("event-2"));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn repeated_events_are_duplicates() {
        let mut cache = EventCache::new();
        assert!(!cache.is_duplicate("event-1"));
        assert!(cache.is_duplicate("event-1"));
        assert!(cache.is_duplicate("event-1"));
    }

    #[test]
    fn different_events_are_independent() {
        let mut cache = EventCache::new();
        assert!(!cache.is_duplicate("event-1"));
        assert!(!cache.is_duplicate("event-2"));
        assert!(cache.is_duplicate("event-1"));
        assert!(cache.is_duplicate("event-2"));
    }
}
