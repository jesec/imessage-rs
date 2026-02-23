/// File watcher and DB pollers for detecting iMessage changes.
///
/// Architecture:
///   1. `notify` crate watches chat.db + chat.db-wal for fs events
///   2. 500ms debounce collapses rapid writes into a single poll
///   3. `MessagePoller` queries for new/updated messages since last check
///   4. `ChatUpdatePoller` queries for read-status changes
///   5. Events are emitted via a tokio broadcast channel
pub mod listener;
pub mod pollers;
