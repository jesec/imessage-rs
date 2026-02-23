use parking_lot::Mutex;
/// File system watcher for chat.db + WAL with debounce and polling orchestration.
///
/// Flow:
///   1. `notify` watches chat.db and chat.db-wal
///   2. FS events are debounced to 500ms
///   3. On each debounced event, acquires a process lock (Semaphore(1))
///   4. Calls MessagePoller and ChatUpdatePoller
///   5. Emits events via tokio broadcast channel
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::{Semaphore, broadcast};
use tracing::{debug, info, warn};

use imessage_db::imessage::repository::MessageRepository;

use crate::pollers::{self, PollerState, WatcherEvent};

/// The iMessage listener watches for file system changes and polls the database.
pub struct IMessageListener {
    /// Path to chat.db
    db_path: PathBuf,
    /// Broadcast channel for events
    event_tx: broadcast::Sender<WatcherEvent>,
    /// Last poll time (Unix ms)
    last_check: Arc<Mutex<i64>>,
}

impl IMessageListener {
    /// Create a new listener.
    ///
    /// `db_path` should be `~/Library/Messages/chat.db`.
    pub fn new(db_path: PathBuf) -> (Self, broadcast::Receiver<WatcherEvent>) {
        let (tx, rx) = broadcast::channel(256);
        let now_ms = current_unix_ms();

        (
            Self {
                db_path,
                event_tx: tx,
                last_check: Arc::new(Mutex::new(now_ms)),
            },
            rx,
        )
    }

    /// Subscribe to events.
    pub fn subscribe(&self) -> broadcast::Receiver<WatcherEvent> {
        self.event_tx.subscribe()
    }

    /// Start the file watcher. This runs until the returned handle is dropped.
    ///
    /// `repo` is the shared MessageRepository (behind a Mutex).
    pub async fn start(&self, repo: Arc<Mutex<MessageRepository>>) -> anyhow::Result<()> {
        let db_path = self.db_path.clone();
        let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));

        info!(
            "Starting iMessage listener on {} and {}",
            db_path.display(),
            wal_path.display()
        );

        // Initial poll (seed caches, don't emit)
        let initial_poller_state = {
            let last_check = *self.last_check.lock();
            let after = last_check - 60_000; // 60s lookback for initial seed
            let repo_lock = repo.lock();
            let mut poller_state = PollerState::new();
            let _ = pollers::poll_messages(&repo_lock, &mut poller_state, after);
            let _ = pollers::poll_chat_reads(&repo_lock, &mut poller_state, after);
            poller_state
        };

        let event_tx = self.event_tx.clone();
        let last_check = self.last_check.clone();
        let process_lock = Arc::new(Semaphore::new(1));

        // Channel for fs events → debounce → poll
        let (fs_tx, mut fs_rx) = tokio::sync::mpsc::channel::<()>(64);

        // Start the notify watcher
        let fs_tx_clone = fs_tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    // Only care about data changes (modify events)
                    if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                        let _ = fs_tx_clone.try_send(());
                    }
                }
                Err(e) => {
                    warn!("File watcher error: {e}");
                }
            }
        })?;

        // Watch both files
        if db_path.exists() {
            watcher.watch(&db_path, RecursiveMode::NonRecursive)?;
        }
        if wal_path.exists() {
            watcher.watch(&wal_path, RecursiveMode::NonRecursive)?;
        }

        info!("iMessage listener started, watching for changes");

        // Poller state lives across polls — seeded from initial poll to avoid re-emitting old messages
        let poller_state = Arc::new(Mutex::new(initial_poller_state));

        // Debounce + poll loop (with periodic fallback every 5s)
        let mut fallback_interval = tokio::time::interval(Duration::from_secs(5));
        fallback_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Skip the first immediate tick
        fallback_interval.tick().await;

        loop {
            // Wait for an fs event OR the fallback timer
            tokio::select! {
                result = fs_rx.recv() => {
                    if result.is_none() {
                        break; // channel closed
                    }
                    // Debounce: wait 500ms, draining any additional events
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    while fs_rx.try_recv().is_ok() {} // drain
                }
                _ = fallback_interval.tick() => {
                    // Periodic fallback — drain any queued fs events too
                    while fs_rx.try_recv().is_ok() {}
                }
            }

            // Acquire the process lock
            let _permit = process_lock.acquire().await.unwrap();

            let now = current_unix_ms();
            let after = {
                let lc = last_check.lock();
                let lookback = *lc - 30_000; // 30s lookback
                // Cap to not exceed now and not be more than 24h old
                let min_bound = now - 86_400_000;
                lookback.max(min_bound).min(now)
            };

            debug!("Polling for changes since {} (now={})", after, now);

            // Run pollers
            let events = {
                let repo_lock = repo.lock();
                let mut ps = poller_state.lock();

                let mut all_events = pollers::poll_messages(&repo_lock, &mut ps, after);
                all_events.extend(pollers::poll_chat_reads(&repo_lock, &mut ps, after));
                ps.trim_caches();
                all_events
            };

            // Update last check time
            *last_check.lock() = now;

            // Emit events
            for event in events {
                debug!("Emitting event: {}", event.event_type);
                let _ = event_tx.send(event);
                // Small delay between events (10ms) to prevent flooding
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            // Brief delay after releasing the lock
            drop(_permit);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Ok(())
    }
}

fn current_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_unix_ms_is_reasonable() {
        let ms = current_unix_ms();
        // Should be after Jan 1, 2024
        assert!(ms > 1_704_067_200_000);
    }

    #[test]
    fn listener_creation() {
        let db_path = PathBuf::from("/tmp/test-chat.db");
        let (listener, _rx) = IMessageListener::new(db_path.clone());
        assert_eq!(listener.db_path, db_path);
    }

    #[test]
    fn subscriber_receives_channel() {
        let (listener, _rx) = IMessageListener::new(PathBuf::from("/tmp/test.db"));
        let _sub = listener.subscribe();
        // Just verify it doesn't panic
    }
}
