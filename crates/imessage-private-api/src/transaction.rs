/// Transaction manager: tracks pending request/response cycles with the helper dylib.
///
/// Each outgoing action that expects a response gets a TransactionPromise with a UUID.
/// The dylib sends back a response with the matching transactionId.
/// Timeout: 120 seconds.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{Mutex, oneshot};
use tracing::warn;

/// Transaction types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionType {
    Chat,
    Message,
    Attachment,
    Handle,
    FindMy,
    Other,
}

/// Result returned when a transaction completes.
#[derive(Debug, Clone)]
pub struct TransactionResult {
    pub transaction_type: TransactionType,
    pub identifier: String,
    pub data: Option<Value>,
}

/// A pending transaction: holds the oneshot sender to resolve the caller.
struct PendingTransaction {
    transaction_type: TransactionType,
    sender: oneshot::Sender<Result<TransactionResult, String>>,
}

/// Manages pending transactions, matching responses to requests by transactionId.
pub struct TransactionManager {
    pending: Arc<Mutex<HashMap<String, PendingTransaction>>>,
}

impl TransactionManager {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create a new transaction, returning (transactionId, receiver).
    /// The receiver will get the result when the dylib responds.
    /// A timeout task is spawned that rejects after 120 seconds.
    pub async fn create(
        &self,
        transaction_type: TransactionType,
    ) -> (String, oneshot::Receiver<Result<TransactionResult, String>>) {
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        {
            let mut pending = self.pending.lock().await;
            pending.insert(
                id.clone(),
                PendingTransaction {
                    transaction_type,
                    sender: tx,
                },
            );
        }

        // Spawn timeout task (120 seconds)
        let pending_clone = self.pending.clone();
        let id_clone = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(120)).await;
            let mut pending = pending_clone.lock().await;
            if let Some(txn) = pending.remove(&id_clone) {
                let _ = txn.sender.send(Err("Transaction timeout".to_string()));
                warn!("Transaction {id_clone} timed out after 120s");
            }
        });

        (id, rx)
    }

    /// Resolve a transaction with the dylib's response.
    pub async fn resolve(&self, transaction_id: &str, identifier: &str, data: Option<Value>) {
        let mut pending = self.pending.lock().await;
        if let Some(txn) = pending.remove(transaction_id) {
            let _ = txn.sender.send(Ok(TransactionResult {
                transaction_type: txn.transaction_type,
                identifier: identifier.to_string(),
                data,
            }));
        }
    }

    /// Reject a transaction with an error message.
    pub async fn reject(&self, transaction_id: &str, error: &str) {
        let mut pending = self.pending.lock().await;
        if let Some(txn) = pending.remove(transaction_id) {
            let _ = txn.sender.send(Err(error.to_string()));
        }
    }

    /// Check if a transaction ID is pending.
    pub async fn is_pending(&self, transaction_id: &str) -> bool {
        let pending = self.pending.lock().await;
        pending.contains_key(transaction_id)
    }

    /// Number of pending transactions.
    pub async fn pending_count(&self) -> usize {
        let pending = self.pending.lock().await;
        pending.len()
    }
}

impl Default for TransactionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_resolve() {
        let mgr = TransactionManager::new();
        let (id, rx) = mgr.create(TransactionType::Message).await;

        assert!(mgr.is_pending(&id).await);

        mgr.resolve(
            &id,
            "msg-guid-123",
            Some(serde_json::json!({"status": "ok"})),
        )
        .await;

        let result = rx.await.unwrap().unwrap();
        assert_eq!(result.identifier, "msg-guid-123");
        assert_eq!(result.transaction_type, TransactionType::Message);
        assert!(result.data.is_some());

        assert!(!mgr.is_pending(&id).await);
    }

    #[tokio::test]
    async fn create_and_reject() {
        let mgr = TransactionManager::new();
        let (id, rx) = mgr.create(TransactionType::Chat).await;

        mgr.reject(&id, "something went wrong").await;

        let result = rx.await.unwrap();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "something went wrong");
    }

    #[tokio::test]
    async fn pending_count() {
        let mgr = TransactionManager::new();
        assert_eq!(mgr.pending_count().await, 0);

        let (id1, _rx1) = mgr.create(TransactionType::Message).await;
        let (_id2, _rx2) = mgr.create(TransactionType::Chat).await;
        assert_eq!(mgr.pending_count().await, 2);

        mgr.resolve(&id1, "x", None).await;
        assert_eq!(mgr.pending_count().await, 1);
    }
}
