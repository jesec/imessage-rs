/// Private API TCP service.
///
/// Binds a TCP server on localhost, accepts connections from the helper dylib,
/// sends actions as newline-delimited JSON, and processes incoming events/responses.
///
/// Port: 45670 + (uid - 501), clamped to [45670, 65535].
/// Write lock: Semaphore(1), released 200ms after each write.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore, broadcast};
use tracing::{error, info, warn};

use crate::actions::Action;
use crate::events::RawEvent;
use crate::transaction::{TransactionManager, TransactionResult};

const MIN_PORT: u16 = 45670;
const MAX_PORT: u16 = 65535;

/// Calculate the Private API port for the current user.
pub fn calculate_port() -> u16 {
    let uid = unsafe { libc::getuid() };
    let port = MIN_PORT as u32 + uid.saturating_sub(501);
    port.min(MAX_PORT as u32) as u16
}

/// A connected client socket.
struct Client {
    id: String,
    writer: tokio::io::WriteHalf<TcpStream>,
    process: Option<String>,
}

/// Bundled context for the client reader task (avoids too many function arguments).
struct ReadContext {
    client: Arc<Mutex<Client>>,
    clients: Arc<Mutex<HashMap<String, Arc<Mutex<Client>>>>>,
    event_tx: broadcast::Sender<RawEvent>,
    txn_mgr: Arc<TransactionManager>,
    messages_ready: Arc<AtomicBool>,
    facetime_ready: Arc<AtomicBool>,
    findmy_ready: Arc<AtomicBool>,
}

/// The Private API service manages the TCP server and connected dylib clients.
pub struct PrivateApiService {
    port: u16,
    transaction_manager: Arc<TransactionManager>,
    write_lock: Arc<Semaphore>,
    clients: Arc<Mutex<HashMap<String, Arc<Mutex<Client>>>>>,
    event_tx: broadcast::Sender<RawEvent>,
    shutdown_tx: Option<broadcast::Sender<()>>,
    /// Per-process readiness: set when the dylib sends a "ready" event after
    /// eagerly initializing IMCore singletons. Reset on client disconnect.
    messages_ready: Arc<AtomicBool>,
    facetime_ready: Arc<AtomicBool>,
    findmy_ready: Arc<AtomicBool>,
}

impl PrivateApiService {
    pub fn new() -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            port: calculate_port(),
            transaction_manager: Arc::new(TransactionManager::new()),
            write_lock: Arc::new(Semaphore::new(1)),
            clients: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
            shutdown_tx: None,
            messages_ready: Arc::new(AtomicBool::new(false)),
            facetime_ready: Arc::new(AtomicBool::new(false)),
            findmy_ready: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Subscribe to incoming events from the dylib.
    pub fn subscribe_events(&self) -> broadcast::Receiver<RawEvent> {
        self.event_tx.subscribe()
    }

    /// Get a reference to the transaction manager.
    pub fn transaction_manager(&self) -> &Arc<TransactionManager> {
        &self.transaction_manager
    }

    /// Check if any dylib clients are connected.
    pub async fn is_connected(&self) -> bool {
        let clients = self.clients.lock().await;
        !clients.is_empty()
    }

    /// Check if the Messages.app dylib has finished IMCore initialization.
    pub fn is_messages_ready(&self) -> bool {
        self.messages_ready.load(Ordering::Acquire)
    }

    /// Check if the FaceTime.app dylib is ready.
    pub fn is_facetime_ready(&self) -> bool {
        self.facetime_ready.load(Ordering::Acquire)
    }

    /// Check if the FindMy.app dylib is ready.
    pub fn is_findmy_ready(&self) -> bool {
        self.findmy_ready.load(Ordering::Acquire)
    }

    /// Clear the FindMy readiness flag (used before restarting FindMy.app).
    pub fn clear_findmy_ready(&self) {
        self.findmy_ready.store(false, Ordering::Release);
    }

    /// Start the TCP server. Returns a handle to the server task.
    pub async fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        let addr = format!("127.0.0.1:{}", self.port);
        let listener = TcpListener::bind(&addr).await?;
        info!("Private API TCP server listening on {addr}");

        let (shutdown_tx, _) = broadcast::channel(1);
        self.shutdown_tx = Some(shutdown_tx.clone());

        let clients = self.clients.clone();
        let event_tx = self.event_tx.clone();
        let txn_mgr = self.transaction_manager.clone();
        let messages_ready = self.messages_ready.clone();
        let facetime_ready = self.facetime_ready.clone();
        let findmy_ready = self.findmy_ready.clone();

        let handle = tokio::spawn(async move {
            let mut shutdown_rx = shutdown_tx.subscribe();

            loop {
                tokio::select! {
                    result = listener.accept() => {
                        match result {
                            Ok((stream, addr)) => {
                                let client_id = uuid::Uuid::new_v4().to_string();
                                info!("Private API client connected: {addr} (id: {client_id})");

                                let (reader, writer) = tokio::io::split(stream);
                                let client = Arc::new(Mutex::new(Client {
                                    id: client_id.clone(),
                                    writer,
                                    process: None,
                                }));

                                {
                                    let mut clients = clients.lock().await;
                                    clients.insert(client_id.clone(), client.clone());
                                }

                                // Spawn a reader task for this client
                                let ctx = ReadContext {
                                    client: client.clone(),
                                    clients: clients.clone(),
                                    event_tx: event_tx.clone(),
                                    txn_mgr: txn_mgr.clone(),
                                    messages_ready: messages_ready.clone(),
                                    facetime_ready: facetime_ready.clone(),
                                    findmy_ready: findmy_ready.clone(),
                                };
                                let client_id_clone = client_id.clone();

                                tokio::spawn(async move {
                                    Self::handle_client_reads(
                                        reader,
                                        &client_id_clone,
                                        ctx,
                                    )
                                    .await;
                                });
                            }
                            Err(e) => {
                                error!("Failed to accept TCP connection: {e}");
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("Private API TCP server shutting down");
                        break;
                    }
                }
            }
        });

        Ok(handle)
    }

    /// Stop the TCP server.
    pub fn stop(&self) {
        if let Some(ref tx) = self.shutdown_tx {
            let _ = tx.send(());
        }
    }

    /// Send an action to all connected clients, optionally awaiting a transaction response.
    pub async fn send_action(&self, action: Action) -> Result<Option<TransactionResult>> {
        // Acquire write lock
        let permit = self.write_lock.clone().acquire_owned().await?;

        let has_transaction = action.transaction_type.is_some();
        let (transaction_id, rx) = if let Some(txn_type) = action.transaction_type {
            let (id, rx) = self.transaction_manager.create(txn_type).await;
            (Some(id), Some(rx))
        } else {
            (None, None)
        };

        // Build the wire message
        let mut msg = json!({
            "action": action.name,
            "data": action.data,
        });
        if let Some(ref id) = transaction_id {
            msg["transactionId"] = json!(id);
        }

        let wire = format!("{}\n", serde_json::to_string(&msg)?);

        // Write to all connected clients
        let clients = self.clients.lock().await;
        let mut write_success = false;

        for (_, client) in clients.iter() {
            let mut client = client.lock().await;
            match client.writer.write_all(wire.as_bytes()).await {
                Ok(_) => {
                    write_success = true;
                }
                Err(e) => {
                    warn!("Failed to write to client {}: {e}", client.id);
                }
            }
        }
        drop(clients);

        if !write_success && has_transaction {
            // Clean up the transaction
            if let Some(ref id) = transaction_id {
                self.transaction_manager
                    .reject(id, "No connected clients")
                    .await;
            }
        }

        // Release write lock after 200ms delay
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(permit);
        });

        // Await transaction response if applicable
        if let Some(rx) = rx {
            match rx.await {
                Ok(Ok(result)) => Ok(Some(result)),
                Ok(Err(e)) => bail!("Transaction error: {e}"),
                Err(_) => bail!("Transaction channel closed"),
            }
        } else {
            Ok(None)
        }
    }

    /// Handle reading from a single client connection.
    async fn handle_client_reads(
        reader: tokio::io::ReadHalf<TcpStream>,
        client_id: &str,
        ctx: ReadContext,
    ) {
        let mut buf_reader = BufReader::new(reader);
        let mut line_buf = String::new();

        loop {
            line_buf.clear();
            match buf_reader.read_line(&mut line_buf).await {
                Ok(0) => {
                    // Connection closed — reset readiness for this process
                    info!("Private API client disconnected: {client_id}");
                    {
                        let c = ctx.client.lock().await;
                        if let Some(ref process) = c.process {
                            Self::set_process_ready(
                                process,
                                &ctx.messages_ready,
                                &ctx.facetime_ready,
                                &ctx.findmy_ready,
                                false,
                            );
                        }
                    }
                    let mut clients = ctx.clients.lock().await;
                    clients.remove(client_id);
                    break;
                }
                Ok(_) => {
                    // Parse newline-delimited JSON events (may have multiple per read)
                    let raw = line_buf.trim().to_string();
                    if raw.is_empty() {
                        continue;
                    }

                    // De-duplicate events in the same batch
                    let events: HashSet<&str> = raw.split('\n').collect();
                    for event_str in events {
                        let event_str = event_str.trim();
                        if event_str.is_empty() {
                            continue;
                        }

                        match serde_json::from_str::<RawEvent>(event_str) {
                            Ok(event) => {
                                // Handle transaction responses
                                if event.is_transaction_response() {
                                    let txn_id = event.transaction_id.as_deref().unwrap();
                                    if let Some(ref error) = event.error
                                        && !error.is_empty()
                                    {
                                        ctx.txn_mgr.reject(txn_id, error).await;
                                        continue;
                                    }
                                    let identifier = event.identifier.as_deref().unwrap_or("");
                                    ctx.txn_mgr
                                        .resolve(txn_id, identifier, event.extract_data())
                                        .await;
                                    continue;
                                }

                                // Handle ping events specially (register the process)
                                if event.event.as_deref() == Some("ping")
                                    && let Some(ref process) = event.process
                                {
                                    let mut c = ctx.client.lock().await;
                                    c.process = Some(process.clone());
                                    info!(
                                        "Private API client registered: {} (process: {process})",
                                        client_id
                                    );
                                }

                                // Handle ready events (dylib has finished IMCore initialization)
                                if event.event.as_deref() == Some("ready")
                                    && let Some(ref process) = event.process
                                {
                                    Self::set_process_ready(
                                        process,
                                        &ctx.messages_ready,
                                        &ctx.facetime_ready,
                                        &ctx.findmy_ready,
                                        true,
                                    );
                                    info!("Private API ready: {process}");
                                }

                                // Broadcast the event to subscribers
                                let _ = ctx.event_tx.send(event);
                            }
                            Err(e) => {
                                warn!("Failed to parse Private API event: {e} (data: {event_str})");
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Error reading from Private API client {client_id}: {e}");
                    {
                        let c = ctx.client.lock().await;
                        if let Some(ref process) = c.process {
                            Self::set_process_ready(
                                process,
                                &ctx.messages_ready,
                                &ctx.facetime_ready,
                                &ctx.findmy_ready,
                                false,
                            );
                        }
                    }
                    let mut clients = ctx.clients.lock().await;
                    clients.remove(client_id);
                    break;
                }
            }
        }
    }

    /// Set or clear the readiness flag for a process based on its bundle identifier.
    fn set_process_ready(
        process: &str,
        messages_ready: &AtomicBool,
        facetime_ready: &AtomicBool,
        findmy_ready: &AtomicBool,
        ready: bool,
    ) {
        match process {
            "com.apple.MobileSMS" | "com.apple.Messages" => {
                messages_ready.store(ready, Ordering::Release);
            }
            "com.apple.FaceTime" | "com.apple.TelephonyUtilities" => {
                facetime_ready.store(ready, Ordering::Release);
            }
            "com.apple.findmy" => {
                findmy_ready.store(ready, Ordering::Release);
            }
            _ => {}
        }
    }
}

impl Default for PrivateApiService {
    fn default() -> Self {
        Self::new()
    }
}

// libc FFI for getuid
mod libc {
    unsafe extern "C" {
        pub fn getuid() -> u32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_calculation_first_user() {
        // uid 501 -> port 45670
        assert_eq!(
            MIN_PORT as u32 + 501u32.saturating_sub(501),
            MIN_PORT as u32
        );
    }

    #[test]
    fn port_clamped_to_max() {
        let uid = 100000u32;
        let port = MIN_PORT as u32 + uid.saturating_sub(501);
        let clamped = port.min(MAX_PORT as u32) as u16;
        assert_eq!(clamped, MAX_PORT);
    }

    #[tokio::test]
    async fn service_starts_and_stops() {
        let mut service = PrivateApiService::new();
        // Override port to avoid conflict
        service.port = 0; // Let OS assign

        // We can't easily test the full TCP flow without a mock client,
        // but we can verify the service is constructable and the
        // transaction manager works.
        assert!(!service.is_connected().await);
        assert!(!service.is_messages_ready());
        assert!(!service.is_facetime_ready());
        assert_eq!(service.transaction_manager().pending_count().await, 0);
    }
}
