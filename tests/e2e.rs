/// E2E integration tests for the Rust imessage-rs HTTP API.
///
/// By default, these tests auto-compile and spawn a fresh server binary. Set
/// `E2E_BASE` to use an externally-managed server instead.
///
/// Run with:
///   E2E_PEER_ADDRS=a@icloud.com,b@icloud.com,c@icloud.com \
///   cargo test --test e2e -- --ignored
///
/// Environment variables:
///   E2E_BASE           - Base URL; if set, skip auto-spawn (you manage the server)
///   E2E_PASSWORD       - Server password (default: test)
///   E2E_PEER_ADDRS     - Required: comma-separated peer addresses (min 2; 3 enables lifecycle tests)
///
/// All tests are #[ignore] by default since they require iMessage.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

// Global lock ensuring tests run serially even without --test-threads=1
static SERIAL: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Declare a serial E2E test. Acquires a global lock so tests run one at a time
/// even without `--test-threads=1`.
macro_rules! e2e_test {
    (fn $name:ident() $body:block) => {
        #[test]
        #[ignore]
        fn $name() {
            let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
            $body
        }
    };
}

// ===========================================================================
// WebhookReceiver — in-process HTTP server that captures webhook POSTs
// ===========================================================================

struct WebhookReceiver {
    url: String,
    events: Arc<Mutex<Vec<Value>>>,
}

/// Lock a Mutex, recovering from poison (prior panic in another test).
fn lock_events(m: &Mutex<Vec<Value>>) -> std::sync::MutexGuard<'_, Vec<Value>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl WebhookReceiver {
    fn start() -> Self {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind webhook receiver");
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}");
        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                // Set a read timeout so threads don't hang forever
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
                let events = Arc::clone(&events_clone);

                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut content_length: usize = 0;

                    // Read request line
                    let mut request_line = String::new();
                    if reader.read_line(&mut request_line).is_err() {
                        return;
                    }

                    // Read headers (case-insensitive content-length extraction)
                    loop {
                        let mut line = String::new();
                        match reader.read_line(&mut line) {
                            Ok(0) => break, // EOF
                            Err(_) => break,
                            _ => {}
                        }
                        if line == "\r\n" || line == "\n" {
                            break;
                        }
                        let lower = line.to_ascii_lowercase();
                        if let Some(val) = lower.strip_prefix("content-length:") {
                            content_length = val.trim().parse().unwrap_or(0);
                        }
                    }

                    // Read body
                    if content_length > 0 {
                        let mut body = vec![0u8; content_length];
                        if reader.read_exact(&mut body).is_ok() {
                            if let Ok(payload) = serde_json::from_slice::<Value>(&body) {
                                lock_events(&events).push(payload);
                            }
                        }
                    }

                    // Respond 200 OK with Connection: close to prevent keep-alive reuse
                    let response =
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });

        Self { url, events }
    }

    /// Take all collected events, clearing the buffer.
    fn drain(&self) -> Vec<Value> {
        std::mem::take(&mut *lock_events(&self.events))
    }

    /// Poll until an event with matching `type` and `data.guid` appears, or panic on timeout.
    fn wait_for_event_with_guid(
        &self,
        event_type: &str,
        guid: &str,
        timeout: std::time::Duration,
    ) -> Value {
        let start = std::time::Instant::now();
        loop {
            {
                let events = lock_events(&self.events);
                if let Some(ev) = events
                    .iter()
                    .find(|ev| ev["type"] == event_type && ev["data"]["guid"] == guid)
                {
                    return ev.clone();
                }
            }
            if start.elapsed() > timeout {
                let events = lock_events(&self.events);
                panic!(
                    "Timed out waiting for webhook event type={event_type} guid={guid}\n\
                     Received events: {events:?}"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }

    /// Poll until an event with matching `type` appears, or panic on timeout.
    fn wait_for_event(&self, event_type: &str, timeout: std::time::Duration) -> Value {
        let start = std::time::Instant::now();
        loop {
            {
                let events = lock_events(&self.events);
                if let Some(ev) = events.iter().find(|ev| ev["type"] == event_type) {
                    return ev.clone();
                }
            }
            if start.elapsed() > timeout {
                let events = lock_events(&self.events);
                panic!(
                    "Timed out waiting for webhook event type={event_type}\n\
                     Received events: {events:?}"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
}

// ===========================================================================
// Auto-spawn server
// ===========================================================================

/// PID of auto-spawned server process, or 0 if not spawned.
static SERVER_PID: AtomicU32 = AtomicU32::new(0);

/// `atexit` handler: send SIGTERM to auto-spawned server.
extern "C" fn kill_server() {
    let pid = SERVER_PID.load(Ordering::SeqCst);
    if pid != 0 {
        eprintln!("[e2e] Stopping server (pid {pid})...");
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
}

/// Check if anything is listening on the given port.
fn port_in_use(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(500),
    )
    .is_ok()
}

/// Check if any `imessage-rs` process is already running (by PID file, then by process name).
/// Returns `Some(description)` if a conflict is detected.
fn detect_existing_server() -> Option<String> {
    // 1. Check the PID file the server writes on startup
    let pid_file = home::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("Library/Application Support/imessage-rs/.imessage-rs.pid");
    if let Ok(contents) = std::fs::read_to_string(&pid_file) {
        if let Ok(pid) = contents.trim().parse::<i32>() {
            if unsafe { libc::kill(pid, 0) } == 0 {
                return Some(format!(
                    "PID file {pid_file:?} points to live process {pid}"
                ));
            }
        }
    }

    // 2. Check for any running process named `imessage-rs` (catches servers started without PID file)
    if let Ok(output) = std::process::Command::new("pgrep")
        .args(["-x", "imessage-rs"])
        .output()
    {
        if output.status.success() {
            let pids = String::from_utf8_lossy(&output.stdout);
            let pids = pids.trim();
            if !pids.is_empty() {
                return Some(format!("imessage-rs process already running (pid {pids})"));
            }
        }
    }

    None
}

/// Spawn a fresh server binary and wait for it to become ready.
/// Returns the base URL. Panics if an existing server is detected.
fn spawn_server(password: &str, webhook_url: &str) -> String {
    let port: u16 = 1234;

    // Check for existing server — process-level check first (catches servers still starting up),
    // then port check as a fallback
    if let Some(conflict) = detect_existing_server() {
        panic!(
            "[e2e] Existing server detected: {conflict}\n\
             Two processes cannot hook into Messages.app simultaneously.\n\
             Either stop it, or set E2E_BASE to use it explicitly."
        );
    }
    if port_in_use(port) {
        panic!(
            "[e2e] Port {port} is already in use (but no imessage-rs process found).\n\
             Something else is bound to the port. Free it or set E2E_BASE."
        );
    }

    let binary = env!("CARGO_BIN_EXE_imessage-rs");
    eprintln!("[e2e] Spawning server: {binary}");
    let mut child = std::process::Command::new(binary)
        .args([
            "--password",
            password,
            "--enable-private-api",
            "true",
            "--enable-facetime-private-api",
            "true",
            "--enable-findmy-private-api",
            "true",
            "--markdown-to-formatting",
            "true",
            "--webhook",
            webhook_url,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| panic!("[e2e] Failed to spawn server: {e}"));

    let pid = child.id();
    SERVER_PID.store(pid, Ordering::SeqCst);
    unsafe {
        libc::atexit(kill_server);
    }

    // Wait for full readiness: HTTP up AND Private API fully initialized.
    // The dylib eagerly initializes IMCore singletons and sends a "ready" event,
    // which the server exposes as private_api_ready in /server/info.
    let base = format!("http://127.0.0.1:{port}/api/v1");
    let info_url = format!("{base}/server/info?password={password}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    eprintln!("[e2e] Waiting for server on port {port}...");
    let mut http_ready = false;
    let mut helper_connected = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => panic!("[e2e] Server exited prematurely: {status}"),
            Err(e) => panic!("[e2e] Failed to check server status: {e}"),
            Ok(None) => {} // still running
        }
        if std::time::Instant::now() > deadline {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            panic!("[e2e] Server did not become ready within 180 seconds");
        }
        match reqwest::blocking::get(&info_url) {
            Ok(resp) if resp.status().is_success() => {
                if !http_ready {
                    eprintln!("[e2e] HTTP ready, waiting for Private API...");
                    http_ready = true;
                }
                if let Ok(json) = resp.json::<Value>() {
                    if !helper_connected && json["data"]["helper_connected"].as_bool() == Some(true)
                    {
                        eprintln!("[e2e] Helper connected, waiting for IMCore initialization...");
                        helper_connected = true;
                    }
                    if json["data"]["private_api_ready"].as_bool() == Some(true)
                        && json["data"]["findmy_private_api_ready"].as_bool() == Some(true)
                    {
                        break;
                    }
                }
            }
            _ => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    // Server is ready — leak the Child handle; atexit will SIGTERM by PID
    std::mem::forget(child);
    eprintln!("[e2e] Server ready (pid {pid})");
    base
}

// ===========================================================================
// Dynamic test configuration
// ===========================================================================

#[allow(dead_code)]
struct TestConfig {
    base: String,
    password: String,
    self_addr: String,
    self_chat: String,
    peers: Vec<String>,
    group_chat: String,
    webhook_receiver: WebhookReceiver,
}

static CONFIG: LazyLock<TestConfig> = LazyLock::new(|| {
    let password = std::env::var("E2E_PASSWORD").unwrap_or_else(|_| "test".to_string());

    // Start webhook receiver first — auto-spawn needs the URL as a CLI arg
    let webhook_receiver = WebhookReceiver::start();

    // If E2E_BASE is set, use the externally-managed server; otherwise auto-spawn
    let base = match std::env::var("E2E_BASE") {
        Ok(url) => {
            eprintln!("[e2e] Using externally-managed server: {url}");
            url
        }
        Err(_) => spawn_server(&password, &webhook_receiver.url),
    };

    let peers: Vec<String> = std::env::var("E2E_PEER_ADDRS")
        .expect("E2E_PEER_ADDRS required (comma-separated, min 2, e.g. a@x.com,b@x.com,c@x.com)")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        peers.len() >= 2,
        "E2E_PEER_ADDRS needs at least 2 addresses, got {}",
        peers.len()
    );

    // Fetch self address from server info
    let info_url = format!("{base}/server/info?password={password}");
    let resp: Value = reqwest::blocking::get(&info_url)
        .expect("Failed to connect to server")
        .json()
        .expect("Invalid JSON from /server/info");
    let self_addr = resp["data"]["detected_imessage"]
        .as_str()
        .expect("/server/info missing detected_imessage — is the server configured?")
        .to_string();
    assert!(!self_addr.is_empty(), "detected_imessage is empty");
    let self_chat = format!("iMessage;-;{self_addr}");

    // Find or create group chat
    let group_chat = find_or_create_group(&base, &password, &peers[0], &peers[1], &self_addr);

    // Seed the self-chat so tests that query it don't depend on pre-existing history.
    // Send a few messages to ensure minimum counts for pagination tests (need >= 6).
    let seed_url = format!("{base}/message/text?password={password}");
    let c = reqwest::blocking::Client::new();
    for i in 0..6 {
        let body = serde_json::json!({
            "chatGuid": &self_chat,
            "message": format!("E2E seed message {i}"),
            "method": "private-api",
        });
        let resp = c
            .post(&seed_url)
            .json(&body)
            .send()
            .expect("seed send failed");
        assert_eq!(resp.status(), 200, "seed message {i} failed");
    }
    // Let the watcher pick up all seed messages before tests start
    std::thread::sleep(std::time::Duration::from_secs(3));

    let server_pid = SERVER_PID.load(Ordering::SeqCst);
    eprintln!("=== E2E TestConfig ===");
    eprintln!(
        "  server:          {}",
        if server_pid != 0 {
            format!("auto-spawned (pid {server_pid})")
        } else {
            "external".to_string()
        }
    );
    eprintln!("  base:            {base}");
    eprintln!("  self_addr:       {self_addr}");
    eprintln!("  self_chat:       {self_chat}");
    eprintln!("  peers:           {peers:?}");
    eprintln!("  group_chat:      {group_chat}");
    eprintln!("  webhook_url:     {}", webhook_receiver.url);
    eprintln!("======================");

    TestConfig {
        base,
        password,
        self_addr,
        self_chat,
        peers,
        group_chat,
        webhook_receiver,
    }
});

/// Find an existing group chat containing exactly the two peers (and self), or create one.
fn find_or_create_group(
    base: &str,
    password: &str,
    peer: &str,
    group_peer: &str,
    self_addr: &str,
) -> String {
    let query_url = format!("{base}/chat/query?password={password}");
    let body = json!({
        "limit": 1000,
        "with": ["participants"],
    });
    let resp = reqwest::blocking::Client::new()
        .post(&query_url)
        .json(&body)
        .send()
        .expect("Failed to query chats");
    let json: Value = resp.json().expect("Invalid JSON from /chat/query");
    let chats = json["data"].as_array().expect("/chat/query data not array");

    // Find a group chat (style == 43) with exactly the expected members.
    // Participants may or may not include self, so accept groups where every
    // participant is one of {peer, group_peer, self} and both peers are present.
    let is_expected = |addr: &str| {
        addr.eq_ignore_ascii_case(peer)
            || addr.eq_ignore_ascii_case(group_peer)
            || addr.eq_ignore_ascii_case(self_addr)
    };
    for chat in chats {
        if chat["style"].as_i64() != Some(43) {
            continue;
        }
        let participants = match chat["participants"].as_array() {
            Some(p) => p,
            None => continue,
        };
        let addrs: Vec<&str> = participants
            .iter()
            .filter_map(|p| p["address"].as_str())
            .collect();
        if addrs.iter().all(|a| is_expected(a))
            && addrs.iter().any(|a| a.eq_ignore_ascii_case(peer))
            && addrs.iter().any(|a| a.eq_ignore_ascii_case(group_peer))
        {
            let guid = chat["guid"].as_str().unwrap().to_string();
            eprintln!("  Found existing group chat: {guid}");
            return guid;
        }
    }

    // No existing group found — create one via Private API
    eprintln!("  No existing group chat found, creating one...");
    let create_url = format!("{base}/chat/new?password={password}");
    let create_body = json!({
        "addresses": [peer, group_peer],
        "message": "E2E test group setup",
        "method": "private-api",
    });
    let resp = reqwest::blocking::Client::new()
        .post(&create_url)
        .json(&create_body)
        .send()
        .expect("Failed to create group chat");
    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON from /chat/new");
    assert_eq!(status, 200, "Failed to create group chat: {json}");
    let guid = json["data"]["guid"]
        .as_str()
        .expect("Created group chat missing guid")
        .to_string();
    eprintln!("  Created new group chat: {guid}");

    // Wait for chat to settle in DB
    std::thread::sleep(std::time::Duration::from_secs(3));
    guid
}

// ===========================================================================
// Helpers
// ===========================================================================

fn url(path: &str) -> String {
    let base = &CONFIG.base;
    let pw = &CONFIG.password;
    if path.contains('?') {
        format!("{base}/{path}&password={pw}")
    } else {
        format!("{base}/{path}?password={pw}")
    }
}

fn run_id() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::new()
}

/// Extract the specific error detail from an error response envelope.
/// Error responses use: `{ "error": { "type": "...", "message": "<detail>" } }`.
fn error_detail(json: &Value) -> &str {
    json["error"]["message"].as_str().unwrap_or("")
}

fn get_json(path: &str) -> Value {
    let resp = reqwest::blocking::get(url(path)).expect("HTTP GET failed");
    assert_eq!(resp.status(), 200, "GET {path} returned {}", resp.status());
    resp.json().expect("Invalid JSON")
}

fn get_raw(path: &str) -> reqwest::blocking::Response {
    reqwest::blocking::get(url(path)).expect("HTTP GET failed")
}

fn post_json(path: &str, body: &Value) -> (u16, Value) {
    let resp = client()
        .post(url(path))
        .json(body)
        .send()
        .expect("HTTP POST failed");
    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON");
    (status, json)
}

fn put_json(path: &str, body: &Value) -> (u16, Value) {
    let resp = client()
        .put(url(path))
        .json(body)
        .send()
        .expect("HTTP PUT failed");
    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON");
    (status, json)
}

fn delete_no_body(path: &str) -> (u16, Value) {
    let resp = client()
        .delete(url(path))
        .send()
        .expect("HTTP DELETE failed");
    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON");
    (status, json)
}

/// A minimal valid 1x1 red PNG for attachment/icon tests.
fn test_png() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // PNG signature
        0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77,
        0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, // IDAT chunk
        0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xe2, 0x21,
        0xbc, 0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, // IEND chunk
        0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}

/// Send a test PNG attachment and return the message GUID.
/// Used by tests that need a known attachment in the DB.
fn send_test_attachment() -> String {
    let id = run_id();
    let form = reqwest::blocking::multipart::Form::new()
        .text("chatGuid", CONFIG.self_chat.clone())
        .text("name", format!("rust-e2e-setup-{id}.png"))
        .text("method", "private-api")
        .part(
            "attachment",
            reqwest::blocking::multipart::Part::bytes(test_png())
                .file_name(format!("rust-e2e-setup-{id}.png"))
                .mime_str("image/png")
                .unwrap(),
        );
    let resp = client()
        .post(url("message/attachment"))
        .multipart(form)
        .send()
        .expect("send_test_attachment POST failed");
    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON");
    assert_eq!(status, 200, "send_test_attachment failed: {json}");
    json["data"]["guid"].as_str().unwrap().to_string()
}

// ===========================================================================
// Health & Info
// ===========================================================================

e2e_test! {
fn e2e_ping() {
    let json = get_json("ping");
    assert_eq!(json["status"], 200);
    assert_eq!(json["data"], "pong");
}
}

e2e_test! {
fn e2e_server_info() {
    let json = get_json("server/info");
    let data = &json["data"];
    assert_eq!(data["private_api"], true);
    assert_eq!(data["helper_connected"], true);
    // Dynamic: just verify both fields are non-empty strings
    assert!(
        data["detected_icloud"].as_str().map_or(false, |s| !s.is_empty()),
        "detected_icloud should be a non-empty string"
    );
    assert!(
        data["detected_imessage"].as_str().map_or(false, |s| !s.is_empty()),
        "detected_imessage should be a non-empty string"
    );
    assert!(data["server_version"].is_string());
    assert!(data["os_version"].is_string());
}
}

e2e_test! {
fn e2e_auth_rejection() {
    let resp = reqwest::blocking::get(format!("{}/ping", CONFIG.base)).expect("HTTP GET failed");
    assert_eq!(resp.status(), 401);
}
}

// ===========================================================================
// Server routes (logs, permissions, update check, restart/soft, alerts)
// ===========================================================================

e2e_test! {
fn e2e_server_logs() {
    let json = get_json("server/logs?count=5");
    assert!(json["data"].is_string(), "logs data should be a string");
}
}

e2e_test! {
fn e2e_server_permissions() {
    let json = get_json("server/permissions");
    let data = json["data"].as_array().expect("permissions should be array");
    assert_eq!(data.len(), 4, "should have 4 permission entries");
    for perm in data {
        assert!(perm["name"].is_string(), "permission should have name");
        assert!(perm["pass"].is_boolean(), "permission should have pass");
        assert!(perm["solution"].is_string(), "permission should have solution");
    }
}
}

// ===========================================================================
// Statistics
// ===========================================================================

e2e_test! {
fn e2e_statistics_totals() {
    let json = get_json("server/statistics/totals");
    let data = &json["data"];
    assert!(data["handles"].is_u64());
    assert!(data["messages"].is_u64());
    assert!(data["chats"].is_u64());
    assert!(data["attachments"].is_u64());
}
}

e2e_test! {
fn e2e_statistics_media() {
    let json = get_json("server/statistics/media");
    let data = &json["data"];
    assert!(data["images"].is_u64());
    assert!(data["videos"].is_u64());
}
}

// ===========================================================================
// Messages (read-only)
// ===========================================================================

e2e_test! {
fn e2e_message_count() {
    let json = get_json("message/count");
    assert!(json["data"]["total"].is_u64());
}
}

e2e_test! {
fn e2e_message_count_updated() {
    let json = get_json("message/count/updated?after=0");
    assert!(json["data"]["total"].is_u64());
}
}

e2e_test! {
fn e2e_message_count_me() {
    let json = get_json("message/count/me");
    assert!(json["data"]["total"].is_u64());
}
}

e2e_test! {
fn e2e_message_query() {
    let body = json!({
        "limit": 3,
        "sort": "DESC",
        "with": ["chat", "chats", "attachment", "attachments"]
    });
    let (status, json) = post_json("message/query", &body);
    assert_eq!(status, 200);
    let data = json["data"].as_array().expect("data should be an array");
    assert!(!data.is_empty(), "should have at least one message");
    assert!(data.len() <= 3, "should respect limit=3, got {}", data.len());
    assert_eq!(json["metadata"]["limit"], 3);
    assert!(json["metadata"]["total"].is_u64(), "metadata should have total");
    assert!(json["metadata"]["offset"].is_u64(), "metadata should have offset");

    let msg = &data[0];
    assert!(msg["guid"].is_string());
    assert!(msg["originalROWID"].is_u64());
    assert!(msg["isFromMe"].is_boolean());
    assert!(msg["dateCreated"].is_u64());
    assert!(msg["attachments"].is_array());
    assert!(msg["chats"].is_array());

    // Verify DESC sort order if multiple messages returned
    if data.len() >= 2 {
        let d0 = data[0]["dateCreated"].as_u64().unwrap_or(0);
        let d1 = data[1]["dateCreated"].as_u64().unwrap_or(0);
        assert!(d0 >= d1, "messages should be in DESC order: {d0} < {d1}");
    }
}
}

e2e_test! {
fn e2e_message_serialization_fields() {
    let body = json!({"limit": 1, "sort": "DESC"});
    let (_, json) = post_json("message/query", &body);
    let msg = &json["data"][0];

    let expected_fields = [
        "originalROWID", "guid", "text", "attributedBody", "handle", "handleId",
        "otherHandle", "attachments", "subject", "error", "dateCreated", "dateRead",
        "dateDelivered", "isDelivered", "isFromMe", "hasDdResults", "isArchived",
        "itemType", "groupTitle", "groupActionType", "balloonBundleId",
        "associatedMessageGuid", "associatedMessageType", "expressiveSendStyleId",
        "threadOriginatorGuid", "hasPayloadData", "country", "isDelayed",
        "isAutoReply", "isSystemMessage", "isServiceMessage", "isForward",
        "threadOriginatorPart", "isCorrupt", "datePlayed", "cacheRoomnames",
        "isSpam", "isExpired", "timeExpressiveSendPlayed", "isAudioMessage",
        "replyToGuid", "shareStatus", "shareDirection", "wasDeliveredQuietly",
        "didNotifyRecipient", "chats", "messageSummaryInfo", "payloadData",
        "dateEdited", "dateRetracted", "partCount",
    ];

    let obj = msg.as_object().expect("message should be an object");
    for field in &expected_fields {
        assert!(obj.contains_key(*field), "missing field: {field}");
    }
}
}

// ===========================================================================
// Handles
// ===========================================================================

e2e_test! {
fn e2e_handle_count() {
    let json = get_json("handle/count");
    assert!(json["data"]["total"].is_u64());
}
}

e2e_test! {
fn e2e_handle_imessage_availability() {
    let addr = &CONFIG.peers[0];

    // GET
    let json = get_json(&format!("handle/availability/imessage?address={addr}"));
    assert!(json["data"]["available"].is_boolean());

    // POST
    let (status, json) = post_json("handle/availability/imessage", &json!({"address": addr}));
    assert_eq!(status, 200);
    assert!(json["data"]["available"].is_boolean());
}
}

e2e_test! {
fn e2e_handle_query() {
    let body = json!({"limit": 3});
    let (status, json) = post_json("handle/query", &body);
    assert_eq!(status, 200);
    let handles = json["data"].as_array().expect("data should be an array");
    assert!(!handles.is_empty(), "should have at least one handle");
    assert!(handles.len() <= 3, "should respect limit=3, got {}", handles.len());
    assert!(json["metadata"]["total"].is_u64());
    assert!(json["metadata"]["offset"].is_u64(), "metadata should have offset");
    // Verify handle shape
    let h = &handles[0];
    assert!(h["address"].is_string());
    assert!(h["service"].is_string());
    assert!(h["originalROWID"].is_u64());
}
}

e2e_test! {
fn e2e_handle_find() {
    let json = get_json(&format!("handle/{}", CONFIG.peers[0]));
    let data = &json["data"];
    assert!(data["address"].is_string());
    assert_eq!(
        data["address"].as_str().unwrap().to_lowercase(),
        CONFIG.peers[0].to_lowercase(),
        "returned address should match queried address"
    );
    assert!(data["originalROWID"].is_u64());
    assert!(data["service"].is_string(), "handle should have service");
}
}

e2e_test! {
fn e2e_handle_query_with_chats() {
    let body = json!({"limit": 3, "with": ["chats"]});
    let (status, json) = post_json("handle/query", &body);
    assert_eq!(status, 200);
    let handles = json["data"].as_array().unwrap();
    assert!(!handles.is_empty());
    // Each handle should have a "chats" array
    assert!(
        handles[0]["chats"].is_array(),
        "handle should have chats array when with=[chats]"
    );
}
}

e2e_test! {
fn e2e_handle_facetime_availability() {
    let addr = &CONFIG.peers[0];

    // GET
    let json = get_json(&format!("handle/availability/facetime?address={addr}"));
    assert!(json["data"]["available"].is_boolean());

    // POST
    let (status, json) = post_json("handle/availability/facetime", &json!({"address": addr}));
    assert_eq!(status, 200);
    assert!(json["data"]["available"].is_boolean());
}
}

e2e_test! {
fn e2e_handle_focus_status() {
    let json = get_json(&format!("handle/{}/focus", CONFIG.peers[0]));
    let status = json["data"]["status"].as_str().expect("focus status should have status string");
    assert!(
        status == "none" || status == "silenced" || status == "unknown",
        "focus status should be none, silenced, or unknown, got: {status}"
    );
}
}

// ===========================================================================
// Chats (1:1)
// ===========================================================================

e2e_test! {
fn e2e_chat_count() {
    let json = get_json("chat/count");
    assert!(json["data"]["total"].is_u64());
}
}

e2e_test! {
fn e2e_chat_query() {
    let body = json!({"limit": 3, "sort": "lastmessage", "with": ["lastMessage"]});
    let (status, json) = post_json("chat/query", &body);
    assert_eq!(status, 200);
    let chats = json["data"].as_array().expect("data should be an array");
    assert!(!chats.is_empty(), "should have at least one chat");
    assert!(chats.len() <= 3, "should respect limit=3, got {}", chats.len());
    // Verify first chat has expected fields
    let chat = &chats[0];
    assert!(chat["guid"].is_string(), "chat should have guid");
    assert!(chat["style"].is_u64(), "chat should have style");
}
}

e2e_test! {
fn e2e_chat_query_with_last_message() {
    let body = json!({
        "limit": 5,
        "sort": "lastmessage",
        "with": ["lastMessage"]
    });
    let (status, json) = post_json("chat/query", &body);
    assert_eq!(status, 200);
    let chats = json["data"].as_array().unwrap();
    assert!(!chats.is_empty(), "should have at least one chat");
    assert!(chats.len() <= 5, "should respect limit=5, got {}", chats.len());
    // All chats should have a lastMessage field
    for (i, chat) in chats.iter().enumerate() {
        assert!(
            chat.get("lastMessage").is_some(),
            "chat[{i}] should have lastMessage when with=[lastMessage]"
        );
    }
    // Verify sort order (descending by lastMessage date)
    if chats.len() >= 2 {
        let d0 = chats[0]["lastMessage"]["dateCreated"].as_u64().unwrap_or(0);
        let d1 = chats[1]["lastMessage"]["dateCreated"].as_u64().unwrap_or(0);
        assert!(d0 >= d1, "chats should be sorted by lastMessage DESC: {d0} < {d1}");
    }
}
}

e2e_test! {
fn e2e_chat_find() {
    // Uses iMessage prefix; on Tahoe the server normalizes to any;
    let json = get_json(&format!("chat/{}", CONFIG.self_chat));
    assert!(json["data"]["guid"].is_string());
}
}

e2e_test! {
fn e2e_chat_messages() {
    let json = get_json(&format!("chat/{}/message?limit=5&sort=DESC", CONFIG.self_chat));
    let msgs = json["data"].as_array().expect("data should be an array");
    assert!(!msgs.is_empty(), "self-chat should have messages");
    assert!(msgs.len() <= 5, "should respect limit=5, got {}", msgs.len());
    // Verify message shape
    let msg = &msgs[0];
    assert!(msg["guid"].is_string(), "message should have guid");
    assert!(msg["dateCreated"].is_u64(), "message should have dateCreated");
    assert!(msg["isFromMe"].is_boolean(), "message should have isFromMe");
    // Verify DESC sort order
    if msgs.len() >= 2 {
        let d0 = msgs[0]["dateCreated"].as_u64().unwrap_or(0);
        let d1 = msgs[1]["dateCreated"].as_u64().unwrap_or(0);
        assert!(d0 >= d1, "messages should be in DESC order: {d0} < {d1}");
    }
}
}

// ===========================================================================
// Private API: Chat operations (1:1)
// ===========================================================================

e2e_test! {
fn e2e_chat_mark_read() {
    let resp = client()
        .post(url(&format!("chat/{}/read", CONFIG.self_chat)))
        .send()
        .expect("POST failed");
    assert_eq!(resp.status(), 200);
}
}

e2e_test! {
fn e2e_chat_mark_unread() {
    let resp = client()
        .post(url(&format!("chat/{}/unread", CONFIG.self_chat)))
        .send()
        .expect("POST failed");
    assert_eq!(resp.status(), 200);
}
}

e2e_test! {
fn e2e_chat_typing() {
    // Start typing
    let resp = client()
        .post(url(&format!("chat/{}/typing", CONFIG.self_chat)))
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Stop typing
    let resp = client()
        .delete(url(&format!("chat/{}/typing", CONFIG.self_chat)))
        .send()
        .unwrap();
    assert_eq!(resp.status(), 200);
}
}

// ===========================================================================
// Private API: Send message (core test)
// ===========================================================================

e2e_test! {
fn e2e_send_text_private_api() {
    let id = run_id();
    CONFIG.webhook_receiver.drain(); // clear stale events

    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Rust E2E #{id}: Private API"),
        "method": "private-api",
        "tempGuid": format!("temp-rust-{id}")
    });

    let (status, json) = post_json("message/text", &body);
    assert_eq!(status, 200, "send failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert_eq!(json["data"]["tempGuid"], format!("temp-rust-{id}"));
    assert!(json["data"]["guid"].is_string());
    assert!(json["data"]["dateCreated"].is_u64());
    assert_eq!(json["data"]["error"], 0);

    // Verify message appears in DB
    let guid = json["data"]["guid"].as_str().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));

    let found = get_json(&format!("message/{guid}?with=chat,attachment"));
    assert_eq!(found["data"]["guid"], guid);
    assert_eq!(found["data"]["isFromMe"], true);

    // Verify webhook delivery
    let wh = CONFIG
        .webhook_receiver
        .wait_for_event_with_guid("new-message", guid, std::time::Duration::from_secs(10));
    assert_eq!(wh["type"], "new-message");
    assert_eq!(wh["data"]["guid"], guid);
}
}

// ===========================================================================
// AppleScript: Send text and attachment
// ===========================================================================

e2e_test! {
fn e2e_send_text_applescript() {
    let id = run_id();

    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Rust E2E #{id}: AppleScript"),
        "method": "apple-script",
        "tempGuid": format!("temp-as-{id}")
    });

    let resp = client()
        .post(url("message/text"))
        .json(&body)
        .timeout(std::time::Duration::from_secs(45))
        .send()
        .expect("HTTP POST failed");
    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON");
    assert_eq!(status, 200, "AppleScript send failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert!(json["data"]["guid"].is_string());
    assert_eq!(json["data"]["error"], 0);
}
}

e2e_test! {
fn e2e_send_attachment_applescript() {
    let id = run_id();

    let form = reqwest::blocking::multipart::Form::new()
        .text("chatGuid", CONFIG.self_chat.clone())
        .text("name", format!("rust-e2e-as-{id}.png"))
        .text("method", "apple-script")
        .text("tempGuid", format!("temp-as-att-{id}"))
        .part(
            "attachment",
            reqwest::blocking::multipart::Part::bytes(test_png())
                .file_name(format!("rust-e2e-as-{id}.png"))
                .mime_str("image/png")
                .unwrap(),
        );

    let resp = client()
        .post(url("message/attachment"))
        .multipart(form)
        .timeout(std::time::Duration::from_secs(45))
        .send()
        .expect("POST failed");

    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON");
    assert_eq!(status, 200, "AppleScript attachment send failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert!(json["data"]["guid"].is_string());
    assert_eq!(json["data"]["error"], 0);
    // tempGuid should be echoed back in the response
    assert_eq!(
        json["data"]["tempGuid"],
        format!("temp-as-att-{id}"),
        "tempGuid should be echoed in response"
    );
    // Attachments array should be present
    assert!(
        json["data"]["attachments"].is_array(),
        "response should include attachments array"
    );
}
}

// ===========================================================================
// Private API: Send attachment
// ===========================================================================

e2e_test! {
fn e2e_send_attachment_private_api() {
    let id = run_id();

    let form = reqwest::blocking::multipart::Form::new()
        .text("chatGuid", CONFIG.self_chat.clone())
        .text("name", format!("rust-e2e-{id}.png"))
        .text("method", "private-api")
        .text("tempGuid", format!("temp-att-{id}"))
        .part(
            "attachment",
            reqwest::blocking::multipart::Part::bytes(test_png())
                .file_name(format!("rust-e2e-{id}.png"))
                .mime_str("image/png")
                .unwrap(),
        );

    let resp = client()
        .post(url("message/attachment"))
        .multipart(form)
        .send()
        .expect("POST failed");

    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON");
    assert_eq!(status, 200, "attachment send failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert!(json["data"]["guid"].is_string());
    assert_eq!(json["data"]["error"], 0);
    // tempGuid should be echoed back in the response
    assert_eq!(
        json["data"]["tempGuid"],
        format!("temp-att-{id}"),
        "tempGuid should be echoed in response"
    );
    // Attachments array should be present
    assert!(
        json["data"]["attachments"].is_array(),
        "response should include attachments array"
    );
}
}

// ===========================================================================
// Private API: Chunked attachment upload
// ===========================================================================

e2e_test! {
fn e2e_send_attachment_chunk() {
    let id = run_id();
    let att_guid = format!("e2e-chunk-{id}");
    let png = test_png();
    let mid = png.len() / 2;
    let chunk0 = png[..mid].to_vec();
    let chunk1 = png[mid..].to_vec();

    // Upload chunk 0 (non-final)
    let form0 = reqwest::blocking::multipart::Form::new()
        .text("chatGuid", CONFIG.self_chat.clone())
        .text("attachmentGuid", att_guid.clone())
        .text("name", format!("rust-chunk-{id}.png"))
        .text("chunkIndex", "0")
        .text("totalChunks", "2")
        .text("isComplete", "false")
        .text("method", "private-api")
        .part(
            "chunk",
            reqwest::blocking::multipart::Part::bytes(chunk0)
                .file_name("chunk0")
                .mime_str("application/octet-stream")
                .unwrap(),
        );

    let resp = client()
        .post(url("message/attachment/chunk"))
        .multipart(form0)
        .send()
        .expect("chunk 0 POST failed");
    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON");
    assert_eq!(status, 200, "chunk 0 upload failed: {json}");
    assert_eq!(json["data"]["chunkIndex"], 0);
    assert_eq!(json["data"]["remainingChunks"], 1);

    // Upload chunk 1 (final, triggers assembly + send)
    let form1 = reqwest::blocking::multipart::Form::new()
        .text("chatGuid", CONFIG.self_chat.clone())
        .text("attachmentGuid", att_guid.clone())
        .text("name", format!("rust-chunk-{id}.png"))
        .text("chunkIndex", "1")
        .text("totalChunks", "2")
        .text("isComplete", "true")
        .text("method", "private-api")
        .part(
            "chunk",
            reqwest::blocking::multipart::Part::bytes(chunk1)
                .file_name("chunk1")
                .mime_str("application/octet-stream")
                .unwrap(),
        );

    let resp = client()
        .post(url("message/attachment/chunk"))
        .multipart(form1)
        .send()
        .expect("chunk 1 POST failed");
    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON");
    assert_eq!(status, 200, "chunk 1 (final) failed: {json}");
    // Final chunk returns the sent message
    assert_eq!(json["data"]["isFromMe"], true);
    assert!(json["data"]["guid"].is_string());
}
}

// ===========================================================================
// Private API: Reactions
// ===========================================================================

e2e_test! {
fn e2e_send_reaction() {
    let id = run_id();
    CONFIG.webhook_receiver.drain();

    let send_body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("React target #{id}"),
        "method": "private-api",
    });
    let (status, send_json) = post_json("message/text", &send_body);
    assert_eq!(status, 200, "send failed: {send_json}");
    let target_guid = send_json["data"]["guid"].as_str().unwrap().to_string();

    // Wait for the watcher to detect the new message BEFORE reacting.
    // This ensures the watcher has the message in its seen_guids cache,
    // so the reaction will be detected as "updated-message" (not "new-message").
    CONFIG.webhook_receiver.wait_for_event_with_guid(
        "new-message",
        &target_guid,
        std::time::Duration::from_secs(15),
    );
    CONFIG.webhook_receiver.drain();

    // Send a "love" reaction
    let react_body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": target_guid,
        "reaction": "love",
    });
    let (status, json) = post_json("message/react", &react_body);
    assert_eq!(status, 200, "reaction failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert!(json["data"]["associatedMessageGuid"].is_string());

    // Verify webhook delivery — reaction is a separate new-message with associatedMessageGuid
    // We need to find the specific new-message whose associatedMessageGuid references the target,
    // since other new-message events (e.g., the reaction response from ourselves) may arrive too.
    {
        let timeout = std::time::Duration::from_secs(15);
        let start = std::time::Instant::now();
        loop {
            {
                let events = lock_events(&CONFIG.webhook_receiver.events);
                if let Some(ev) = events.iter().find(|ev| {
                    ev["type"] == "new-message"
                        && ev["data"]["associatedMessageGuid"]
                            .as_str()
                            .map_or(false, |g| g.contains(&target_guid))
                }) {
                    assert_eq!(ev["type"], "new-message");
                    break;
                }
            }
            if start.elapsed() > timeout {
                let events = lock_events(&CONFIG.webhook_receiver.events);
                panic!(
                    "Timed out waiting for reaction new-message referencing {target_guid}\n\
                     Received events: {events:?}"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }

    std::thread::sleep(std::time::Duration::from_secs(2));

    // Remove the reaction
    let unreact_body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": target_guid,
        "reaction": "-love",
    });
    let (status, json) = post_json("message/react", &unreact_body);
    assert_eq!(status, 200, "unreaction failed: {json}");
}
}

e2e_test! {
fn e2e_send_emoji_reaction() {
    let id = run_id();
    CONFIG.webhook_receiver.drain();

    // Send a target message
    let send_body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Emoji react target #{id}"),
        "method": "private-api",
    });
    let (status, send_json) = post_json("message/text", &send_body);
    assert_eq!(status, 200, "send failed: {send_json}");
    let target_guid = send_json["data"]["guid"].as_str().unwrap().to_string();

    CONFIG.webhook_receiver.wait_for_event_with_guid(
        "new-message",
        &target_guid,
        std::time::Duration::from_secs(15),
    );
    CONFIG.webhook_receiver.drain();

    // Send an emoji reaction
    let react_body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": target_guid,
        "reaction": "\u{1F440}", // 👀
    });
    let (status, json) = post_json("message/react", &react_body);
    assert_eq!(status, 200, "emoji reaction failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert!(json["data"]["associatedMessageGuid"].is_string());
    assert_eq!(
        json["data"]["associatedMessageType"], "emoji",
        "associatedMessageType should be 'emoji': {json}"
    );
    assert_eq!(
        json["data"]["associatedMessageEmoji"], "\u{1F440}",
        "associatedMessageEmoji should be the emoji: {json}"
    );

    // Verify the reaction is readable via GET
    let react_guid = json["data"]["guid"].as_str().unwrap();
    let fetched = get_json(&format!("message/{react_guid}"));
    assert_eq!(fetched["data"]["associatedMessageType"], "emoji");
    assert_eq!(fetched["data"]["associatedMessageEmoji"], "\u{1F440}");

    std::thread::sleep(std::time::Duration::from_secs(2));

    // Remove the emoji reaction
    let unreact_body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": target_guid,
        "reaction": "-\u{1F440}", // -👀
    });
    let (status, json) = post_json("message/react", &unreact_body);
    assert_eq!(status, 200, "emoji unreaction failed: {json}");
    assert_eq!(
        json["data"]["associatedMessageType"], "-emoji",
        "removal associatedMessageType should be '-emoji': {json}"
    );
}
}

e2e_test! {
fn e2e_send_emoji_reaction_zwj() {
    let id = run_id();
    CONFIG.webhook_receiver.drain();

    // Send a target message
    let send_body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("ZWJ emoji target #{id}"),
        "method": "private-api",
    });
    let (status, send_json) = post_json("message/text", &send_body);
    assert_eq!(status, 200, "send failed: {send_json}");
    let target_guid = send_json["data"]["guid"].as_str().unwrap().to_string();

    CONFIG.webhook_receiver.wait_for_event_with_guid(
        "new-message",
        &target_guid,
        std::time::Duration::from_secs(15),
    );
    CONFIG.webhook_receiver.drain();

    // Send a ZWJ sequence emoji (woman technologist: 👩‍💻)
    let react_body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": target_guid,
        "reaction": "\u{1F469}\u{200D}\u{1F4BB}",
    });
    let (status, json) = post_json("message/react", &react_body);
    assert_eq!(status, 200, "ZWJ emoji reaction failed: {json}");
    assert_eq!(json["data"]["associatedMessageType"], "emoji");
    assert_eq!(json["data"]["associatedMessageEmoji"], "\u{1F469}\u{200D}\u{1F4BB}");
}
}

e2e_test! {
fn e2e_send_sticker_reaction() {
    let id = run_id();
    CONFIG.webhook_receiver.drain();

    // Send a target message
    let send_body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Sticker react target #{id}"),
        "method": "private-api",
    });
    let (status, send_json) = post_json("message/text", &send_body);
    assert_eq!(status, 200, "send failed: {send_json}");
    let target_guid = send_json["data"]["guid"].as_str().unwrap().to_string();

    CONFIG.webhook_receiver.wait_for_event_with_guid(
        "new-message",
        &target_guid,
        std::time::Duration::from_secs(15),
    );
    CONFIG.webhook_receiver.drain();

    // Minimal 1x1 red PNG (base64)
    let tiny_png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==";
    let data_url = format!("data:image/png;base64,{tiny_png}");

    // Send a sticker reaction
    let react_body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": target_guid,
        "reaction": "sticker",
        "sticker": data_url,
    });
    let (status, json) = post_json("message/react", &react_body);
    assert_eq!(status, 200, "sticker reaction failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert_eq!(
        json["data"]["associatedMessageType"], "sticker-tapback",
        "associatedMessageType should be 'sticker-tapback': {json}"
    );

    // Verify the sticker reaction is readable via GET
    let react_guid = json["data"]["guid"].as_str().unwrap();
    let fetched = get_json(&format!("message/{react_guid}"));
    assert_eq!(fetched["data"]["associatedMessageType"], "sticker-tapback");

    std::thread::sleep(std::time::Duration::from_secs(2));

    // Remove the sticker reaction
    let unreact_body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": target_guid,
        "reaction": "-sticker",
    });
    let (status, json) = post_json("message/react", &unreact_body);
    assert_eq!(status, 200, "sticker unreaction failed: {json}");
    assert_eq!(
        json["data"]["associatedMessageType"], "-sticker-tapback",
        "removal associatedMessageType should be '-sticker-tapback': {json}"
    );
}
}

// ===========================================================================
// Private API: Edit message
// ===========================================================================

e2e_test! {
fn e2e_edit_message() {
    let id = run_id();
    CONFIG.webhook_receiver.drain();

    let send_body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Original #{id}"),
        "method": "private-api",
    });
    let (status, send_json) = post_json("message/text", &send_body);
    assert_eq!(status, 200);
    let guid = send_json["data"]["guid"].as_str().unwrap().to_string();

    // Wait for the watcher to detect the new message BEFORE editing.
    // This ensures the watcher has the message in its seen_guids cache,
    // so the edit will be detected as "updated-message" (not "new-message").
    CONFIG.webhook_receiver.wait_for_event_with_guid(
        "new-message",
        &guid,
        std::time::Duration::from_secs(15),
    );
    CONFIG.webhook_receiver.drain();

    let edit_body = json!({
        "editedMessage": format!("Edited #{id}"),
        "backwardsCompatibilityMessage": format!("Edited to \"Edited #{id}\""),
    });
    let (status, json) = post_json(&format!("message/{guid}/edit"), &edit_body);
    assert_eq!(status, 200, "edit failed: {json}");
    assert!(
        json["data"]["dateEdited"].is_u64(),
        "dateEdited should be set after edit"
    );

    // Re-fetch the message and verify the edited text is present
    std::thread::sleep(std::time::Duration::from_secs(2));
    let refetch = get_json(&format!("message/{guid}"));
    let text = refetch["data"]["text"].as_str().unwrap_or("");
    assert!(
        text.contains(&format!("Edited #{id}")),
        "re-fetched message text should contain edited content, got: {text}"
    );

    // Verify webhook delivery for the edit
    let wh = CONFIG.webhook_receiver.wait_for_event_with_guid(
        "updated-message",
        &guid,
        std::time::Duration::from_secs(15),
    );
    assert_eq!(wh["type"], "updated-message");
    assert_eq!(wh["data"]["guid"], guid);
}
}

// ===========================================================================
// Private API: Unsend message
// ===========================================================================

e2e_test! {
fn e2e_unsend_message() {
    let id = run_id();
    CONFIG.webhook_receiver.drain();

    let send_body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("To be unsent #{id}"),
        "method": "private-api",
    });
    let (status, send_json) = post_json("message/text", &send_body);
    assert_eq!(status, 200);
    let guid = send_json["data"]["guid"].as_str().unwrap().to_string();

    // Wait for the watcher to detect the new message BEFORE unsending.
    // This ensures the watcher has the message in its seen_guids cache,
    // so the unsend will be detected as "updated-message" (not "new-message").
    CONFIG.webhook_receiver.wait_for_event_with_guid(
        "new-message",
        &guid,
        std::time::Duration::from_secs(15),
    );
    CONFIG.webhook_receiver.drain();

    let (status, json) = post_json(&format!("message/{guid}/unsend"), &json!({}));
    assert_eq!(status, 200, "unsend failed: {json}");
    // On macOS Tahoe, unsending a self-message sets dateEdited (not dateRetracted)
    assert!(
        json["data"]["dateEdited"].is_u64() || json["data"]["dateRetracted"].is_u64(),
        "dateEdited or dateRetracted should be set after unsend"
    );

    // Re-fetch and verify the message shows as retracted/edited
    std::thread::sleep(std::time::Duration::from_secs(2));
    let refetch = get_json(&format!("message/{guid}"));
    assert!(
        refetch["data"]["dateEdited"].is_u64() || refetch["data"]["dateRetracted"].is_u64(),
        "re-fetched message should have dateEdited or dateRetracted set"
    );

    // Verify webhook delivery for the unsend
    let wh = CONFIG.webhook_receiver.wait_for_event_with_guid(
        "updated-message",
        &guid,
        std::time::Duration::from_secs(10),
    );
    assert_eq!(wh["type"], "updated-message");
    assert_eq!(wh["data"]["guid"], guid);
}
}

// ===========================================================================
// Private API: Subject line
// ===========================================================================

e2e_test! {
fn e2e_send_with_subject() {
    let id = run_id();

    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Body #{id}"),
        "subject": format!("Subject #{id}"),
        "method": "private-api",
    });
    let (status, json) = post_json("message/text", &body);
    assert_eq!(status, 200, "send with subject failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert_eq!(json["data"]["subject"], format!("Subject #{id}"));
}
}

// ===========================================================================
// Private API: Reply threading
// ===========================================================================

e2e_test! {
fn e2e_send_reply() {
    let id = run_id();

    let send_body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Thread origin #{id}"),
        "method": "private-api",
    });
    let (status, send_json) = post_json("message/text", &send_body);
    assert_eq!(status, 200);
    let origin_guid = send_json["data"]["guid"].as_str().unwrap().to_string();

    std::thread::sleep(std::time::Duration::from_secs(2));

    let reply_body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Reply #{id}"),
        "method": "private-api",
        "selectedMessageGuid": origin_guid,
        "partIndex": 0,
    });
    let (status, json) = post_json("message/text", &reply_body);
    assert_eq!(status, 200, "reply failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert!(
        json["data"]["threadOriginatorGuid"].is_string(),
        "reply should have threadOriginatorGuid"
    );
}
}

// ===========================================================================
// Private API: Effect ID (expressive send)
// ===========================================================================

e2e_test! {
fn e2e_send_with_effect() {
    let id = run_id();

    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Slam #{id}"),
        "method": "private-api",
        "effectId": "com.apple.MobileSMS.expressivesend.impact",
    });
    let (status, json) = post_json("message/text", &body);
    assert_eq!(status, 200, "send with effect failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert_eq!(
        json["data"]["expressiveSendStyleId"],
        "com.apple.MobileSMS.expressivesend.impact"
    );
}
}

// ===========================================================================
// Private API: Send cache dedup
// ===========================================================================

e2e_test! {
fn e2e_send_cache_dedup() {
    let id = run_id();
    let temp_guid = format!("dedup-test-{id}");

    // Fire two concurrent sends with the same tempGuid.
    // The first should succeed; the second should be rejected while the first is in-flight.
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Dedup test #{id}"),
        "method": "private-api",
        "tempGuid": &temp_guid,
    });

    let body_clone = body.clone();
    let t1 = std::thread::spawn(move || post_json("message/text", &body_clone));

    // Small delay so thread 1 gets cached first
    std::thread::sleep(std::time::Duration::from_millis(200));
    let (status2, json2) = post_json("message/text", &body);
    let (status1, json1) = t1.join().unwrap();

    // Exactly one should succeed and one should be rejected
    let one_succeeded = status1 == 200 || status2 == 200;
    let one_rejected = status1 != 200 || status2 != 200;
    assert!(
        one_succeeded && one_rejected,
        "expected one success and one rejection, got status1={status1} status2={status2}\n\
         json1={json1}\njson2={json2}"
    );
}
}

// ===========================================================================
// Private API: Multipart send
// ===========================================================================

e2e_test! {
fn e2e_send_multipart() {
    let id = run_id();

    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "parts": [
            {"partIndex": 0, "text": format!("Part A #{id}")},
            {"partIndex": 1, "text": format!("Part B #{id}")},
        ],
        "tempGuid": format!("temp-multi-{id}"),
    });
    let (status, json) = post_json("message/multipart", &body);
    assert_eq!(status, 200, "multipart send failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert!(json["data"]["guid"].is_string());
}
}

// ===========================================================================
// Private API: Notify silenced message
// ===========================================================================

e2e_test! {
fn e2e_message_notify() {
    let id = run_id();

    // Send a message first
    let send_body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Notify test #{id}"),
        "method": "private-api",
    });
    let (status, send_json) = post_json("message/text", &send_body);
    assert_eq!(status, 200);
    let guid = send_json["data"]["guid"].as_str().unwrap();

    std::thread::sleep(std::time::Duration::from_secs(2));

    // Notify the message — fresh message so did_notify_recipient is false
    let resp = client()
        .post(url(&format!("message/{guid}/notify")))
        .json(&json!({}))
        .send()
        .expect("POST failed");
    let status = resp.status().as_u16();
    let body: Value = resp.json().expect("Invalid JSON");
    assert_eq!(status, 200, "notify should return 200 for fresh message: {body}");
    // Verify response envelope is well-formed
    assert!(body["status"].is_u64(), "response should have status: {body}");
    assert!(body["message"].is_string(), "response should have message: {body}");
}
}

// ===========================================================================
// Private API: Text formatting — explicit textFormatting array
// ===========================================================================

e2e_test! {
fn e2e_send_with_explicit_formatting() {
    let id = run_id();

    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Hello bold #{id}"),
        "method": "private-api",
        "textFormatting": [
            {"start": 0, "length": 5, "styles": ["bold"]},
        ],
    });
    let (status, json) = post_json("message/text", &body);
    assert_eq!(status, 200, "send with explicit formatting failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert!(json["data"]["guid"].is_string());

    // The send response uses for_sent_message() which parses attributedBody
    assert!(
        !json["data"]["attributedBody"].is_null(),
        "send response attributedBody should be set for formatted message"
    );

    // Verify the GET endpoint also returns attributedBody when requested
    let guid = json["data"]["guid"].as_str().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));

    let found = get_json(&format!("message/{guid}?with=attributedbody"));
    assert_eq!(found["data"]["guid"], guid);
    assert!(
        !found["data"]["attributedBody"].is_null(),
        "GET attributedBody should be set when requested via ?with=attributedbody"
    );
}
}

// ===========================================================================
// Private API: Text formatting — auto markdown conversion
// ===========================================================================

e2e_test! {
fn e2e_send_with_markdown_auto() {
    let id = run_id();

    // Send markdown text; server should strip markers and apply formatting.
    // Requires server started with --markdown-to-formatting true.
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("**bold** and *italic* #{id}"),
        "method": "private-api",
    });
    let (status, json) = post_json("message/text", &body);
    assert_eq!(status, 200, "send with markdown auto failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);

    // The returned text should have markdown markers stripped
    let text = json["data"]["text"].as_str().unwrap_or("");
    assert!(
        !text.contains("**") && !text.contains("*italic*"),
        "markdown markers should be stripped, got: {text}"
    );
    assert!(
        text.contains("bold") && text.contains("italic"),
        "clean text should contain the words, got: {text}"
    );

    // The send response uses for_sent_message() which parses attributedBody
    assert!(
        !json["data"]["attributedBody"].is_null(),
        "send response attributedBody should be set for auto-formatted markdown"
    );

    // Verify via GET with ?with=attributedbody
    let guid = json["data"]["guid"].as_str().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));

    let found = get_json(&format!("message/{guid}?with=attributedbody"));
    assert!(
        !found["data"]["attributedBody"].is_null(),
        "GET attributedBody should be set for auto-formatted markdown message"
    );
}
}

// ===========================================================================
// Private API: Text formatting — conflict rejected
// ===========================================================================

e2e_test! {
fn e2e_formatting_conflict_rejected() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": "conflict test",
        "method": "private-api",
        "textFormatting": [{"start": 0, "length": 4, "styles": ["bold"]}],
        "attributedBody": [{"text": "conflict test"}],
    });
    let (status, _json) = post_json("message/text", &body);
    assert_eq!(
        status, 400,
        "sending both textFormatting and attributedBody should be rejected"
    );
}
}

// ===========================================================================
// Private API: Embedded media
// ===========================================================================

e2e_test! {
fn e2e_embedded_media() {
    // Send a text message, then try to get embedded media. Plain text has no embedded
    // media, so Private API may return null/error or the request may time out.
    let id = run_id();
    let send_body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": format!("Embedded media test #{id}"),
        "method": "private-api",
    });
    let (status, send_json) = post_json("message/text", &send_body);
    assert_eq!(status, 200);
    let guid = send_json["data"]["guid"].as_str().unwrap();

    std::thread::sleep(std::time::Duration::from_secs(2));

    // Use a short timeout since the Private API may hang for text messages
    let tc = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap();
    let result = tc.get(url(&format!("message/{guid}/embedded-media"))).send();
    let resp = result.expect("embedded-media request failed");
    let status = resp.status().as_u16();
    // Plain text messages have no balloonBundleId; handler returns 400 before hitting Private API
    assert_eq!(
        status, 400,
        "embedded-media for plain text should be 400, got {status}"
    );
}
}

// ===========================================================================
// Response envelope
// ===========================================================================

e2e_test! {
fn e2e_response_envelope() {
    let json = get_json("ping");
    assert_eq!(json["status"], 200);
    assert!(json["message"].is_string());
}
}

e2e_test! {
fn e2e_404_plain_text() {
    let resp = reqwest::blocking::get(url("nonexistent")).expect("GET failed");
    assert_eq!(resp.status(), 404);
    assert_eq!(resp.text().unwrap(), "Not Found");
}
}

e2e_test! {
fn e2e_bad_request() {
    let body = json!({"chatGuid": ""});
    let (status, json) = post_json("message/text", &body);
    assert_eq!(status, 400);
    assert!(json["message"].is_string());
}
}

// ===========================================================================
// Pretty JSON middleware
// ===========================================================================

e2e_test! {
fn e2e_pretty_json() {
    let resp = get_raw("ping?pretty");
    assert_eq!(resp.status(), 200);
    let body = resp.text().unwrap();
    // Pretty-printed JSON has newlines and indentation
    assert!(body.contains('\n'), "pretty JSON should contain newlines");
    // Should still be valid JSON
    let parsed: Value =
        serde_json::from_str(&body).expect("pretty response should be valid JSON");
    assert_eq!(parsed["data"], "pong");
}
}

// ===========================================================================
// Webhook list
// ===========================================================================

e2e_test! {
fn e2e_webhook_list() {
    let json = get_json("webhook");
    let all = json["data"].as_array().expect("webhook data should be array");
    assert!(!all.is_empty(), "should have at least the test receiver webhook");
    assert!(
        all.iter().any(|w| w["url"].as_str().map_or(false, |u| u.contains("127.0.0.1"))),
        "should find the test webhook receiver"
    );
}
}

// ===========================================================================
// Attachment routes
// ===========================================================================

e2e_test! {
fn e2e_attachment_count() {
    let json = get_json("attachment/count");
    assert!(json["data"]["total"].is_u64());
}
}

e2e_test! {
fn e2e_attachment_find_and_download() {
    // Send our own attachment so this test is self-contained
    let msg_guid = send_test_attachment();
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Fetch the message to get the attachment GUID
    let msg_json = get_json(&format!("message/{msg_guid}?with=attachment"));
    let att_guid = msg_json["data"]["attachments"][0]["guid"]
        .as_str()
        .expect("sent message should have an attachment");

    // Find attachment by GUID
    let att_json = get_json(&format!("attachment/{att_guid}"));
    assert!(att_json["data"]["guid"].is_string());
    assert!(
        att_json["data"]["mimeType"].is_string() || att_json["data"]["mimeType"].is_null()
    );

    // Download the freshly-sent attachment
    let resp = get_raw(&format!("attachment/{att_guid}/download"));
    assert_eq!(
        resp.status().as_u16(),
        200,
        "download should return 200 for fresh attachment"
    );
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(!ct.contains("json"), "download should not return JSON");
}
}

e2e_test! {
fn e2e_attachment_upload() {
    let id = run_id();
    let data = b"e2e test file content";

    let form = reqwest::blocking::multipart::Form::new().part(
        "attachment",
        reqwest::blocking::multipart::Part::bytes(data.to_vec())
            .file_name(format!("e2e-upload-{id}.txt"))
            .mime_str("text/plain")
            .unwrap(),
    );

    let resp = client()
        .post(url("attachment/upload"))
        .multipart(form)
        .send()
        .expect("upload failed");

    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON");
    assert_eq!(status, 200, "upload failed: {json}");
    assert!(json["data"]["path"].is_string(), "upload should return path");
}
}

e2e_test! {
fn e2e_attachment_blurhash_not_found() {
    // Non-existent attachment GUID should return 404
    let resp = get_raw("attachment/some-guid/blurhash");
    assert_eq!(resp.status(), 404);
}
}

e2e_test! {
fn e2e_attachment_blurhash() {
    // Send our own PNG so this test is self-contained
    let msg_guid = send_test_attachment();
    std::thread::sleep(std::time::Duration::from_secs(2));

    let msg_json = get_json(&format!("message/{msg_guid}?with=attachment"));
    let att_guid = msg_json["data"]["attachments"][0]["guid"]
        .as_str()
        .expect("sent message should have an attachment");

    let json = get_json(&format!("attachment/{att_guid}/blurhash"));
    assert_eq!(json["status"], 200);
    let blurhash = json["data"]["blurhash"].as_str().expect("should have blurhash string");
    assert!(!blurhash.is_empty(), "blurhash should not be empty");
}
}

e2e_test! {
fn e2e_attachment_live_photo() {
    // Send our own PNG so this test is self-contained
    let msg_guid = send_test_attachment();
    std::thread::sleep(std::time::Duration::from_secs(2));

    let msg_json = get_json(&format!("message/{msg_guid}?with=attachment"));
    let att_guid = msg_json["data"]["attachments"][0]["guid"]
        .as_str()
        .expect("sent message should have an attachment");

    let resp = get_raw(&format!("attachment/{att_guid}/live"));
    // Our test PNGs have no Live Photo companion (.mov), so 404 is expected
    assert_eq!(
        resp.status().as_u16(),
        404,
        "test PNG should not have a live photo companion"
    );
}
}

// ===========================================================================
// Attachment serializer fields (hasLivePhoto, height, width)
// ===========================================================================

e2e_test! {
fn e2e_attachment_serializer_fields() {
    // Send our own attachment so this test is self-contained
    let msg_guid = send_test_attachment();
    std::thread::sleep(std::time::Duration::from_secs(2));

    let msg_json = get_json(&format!("message/{msg_guid}?with=attachment"));
    let att = &msg_json["data"]["attachments"][0];
    let obj = att.as_object().expect("attachment should be object");
    // These fields should always be present in serialized attachments
    assert!(
        obj.contains_key("hasLivePhoto"),
        "attachment should have hasLivePhoto field"
    );
    assert!(
        obj.contains_key("height"),
        "attachment should have height field"
    );
    assert!(
        obj.contains_key("width"),
        "attachment should have width field"
    );
    assert!(
        att["hasLivePhoto"].is_boolean(),
        "hasLivePhoto should be boolean"
    );
}
}

// ===========================================================================
// iCloud routes
// ===========================================================================

e2e_test! {
fn e2e_icloud_account() {
    let json = get_json("icloud/account");
    // data may be null or an object with account info
    assert_eq!(json["status"], 200);
}
}

e2e_test! {
fn e2e_icloud_contact() {
    let addr = &CONFIG.peers[0];
    let json = get_json(&format!("icloud/contact?address={addr}"));
    assert_eq!(json["status"], 200);
    assert!(
        json["message"].is_string(),
        "response should have message field"
    );
}
}

e2e_test! {
fn e2e_icloud_change_alias_validation() {
    // Empty alias should return 400
    let (status, json) = post_json("icloud/account/alias", &json!({"alias": ""}));
    assert_eq!(status, 400, "empty alias should return 400: {json}");
}
}

e2e_test! {
fn e2e_icloud_findmy_devices() {
    // GET reads and decrypts Devices.data from disk (lazy-fetches key on first call).
    // Needs a longer timeout — FindMy.app must connect and report ready for key fetch.
    let client = client();
    let resp = client
        .get(url("icloud/findmy/devices"))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .expect("FindMy devices GET failed");
    let json: Value = resp.json().expect("Invalid JSON");
    assert_eq!(json["status"], 200, "findmy/devices should succeed: {json}");
}
}

e2e_test! {
fn e2e_icloud_findmy_devices_refresh() {
    // POST refresh restarts FindMy.app, waits for reconnect + 10s, then re-reads Devices.data
    let client = client();
    let resp = client
        .post(url("icloud/findmy/devices/refresh"))
        .json(&json!({}))
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .expect("FindMy devices refresh failed");
    let json: Value = resp.json().expect("Invalid JSON");
    assert_eq!(json["status"], 200, "findmy/devices/refresh should succeed: {json}");
}
}

e2e_test! {
fn e2e_icloud_findmy_friends() {
    let json = get_json("icloud/findmy/friends");
    assert_eq!(json["status"], 200);
    assert!(
        json["data"].is_array(),
        "findmy/friends data should be array"
    );
}
}

e2e_test! {
fn e2e_icloud_findmy_friends_refresh() {
    // Refresh triggers the Private API to poll FindMy locations.
    // This is a slow operation (up to 15s timeout inside the dylib).
    let client = client();
    let resp = client
        .post(url("icloud/findmy/friends/refresh"))
        .json(&json!({}))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .expect("FindMy friends refresh HTTP POST failed");
    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON from findmy refresh");

    assert_eq!(status, 200, "findmy/friends/refresh should succeed: {json}");
    assert!(
        json["data"].is_array(),
        "findmy/friends/refresh data should be array, got: {}",
        json["data"]
    );
    assert_eq!(
        json["message"].as_str().unwrap_or(""),
        "Successfully refreshed FindMy friends!",
        "should return success message"
    );
}
}

e2e_test! {
fn e2e_icloud_findmy_friends_refresh_populates_cache() {
    // After refresh, GET /findmy/friends should return the same (or superset) data
    let client = client();
    let refresh_resp = client
        .post(url("icloud/findmy/friends/refresh"))
        .json(&json!({}))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .expect("FindMy friends refresh failed");
    assert_eq!(refresh_resp.status(), 200);
    let refresh_json: Value = refresh_resp.json().expect("Invalid JSON");
    let refresh_data = refresh_json["data"].as_array().expect("refresh data should be array");

    // GET the cache — should have at least as many entries as the refresh returned
    let cache_json = get_json("icloud/findmy/friends");
    let cache_data = cache_json["data"].as_array().expect("cache data should be array");
    assert!(
        cache_data.len() >= refresh_data.len(),
        "cache should have >= refresh entries ({} < {})",
        cache_data.len(),
        refresh_data.len()
    );

    // If we got any locations, validate the shape
    for loc in cache_data {
        assert!(
            loc.get("handle").is_some(),
            "each location should have a 'handle' field, got: {loc}"
        );
    }
}
}

// ===========================================================================
// FaceTime
// ===========================================================================

// POST /api/v1/facetime/session → 200, data has "link" key
// Requires FaceTime dylib to be injected into FaceTime.app.
// The server sends callUUID: null to generate a new link (not tied to an existing call).
e2e_test! {
fn e2e_facetime_session() {
    // Use a longer timeout — FaceTime link generation can take a few seconds
    let c = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap();
    let resp = c
        .post(url("facetime/session"))
        .json(&json!({}))
        .send();

    let r = resp.expect("facetime session: request failed");
    let status = r.status().as_u16();
    let body: Value = r.json().unwrap_or_default();
    assert_eq!(status, 200, "facetime session failed: {body}");
    let link = body["data"]["link"]
        .as_str()
        .expect("missing link in data");
    assert!(
        link.starts_with("https://facetime.apple.com/"),
        "link should be a FaceTime URL: {link}"
    );
}
}

// ===========================================================================
// Group Chat Operations
// ===========================================================================

e2e_test! {
fn e2e_group_chat_find() {
    let guid = &CONFIG.group_chat;
    let json = get_json(&format!("chat/{guid}"));
    let data = &json["data"];
    assert!(data["guid"].is_string(), "group chat should have guid");
    // Verify it's a group chat (style == 43)
    assert_eq!(data["style"], 43, "group chat should have style 43");
}
}

e2e_test! {
fn e2e_group_chat_messages() {
    let guid = &CONFIG.group_chat;
    let json = get_json(&format!("chat/{guid}/message?limit=5&sort=DESC"));
    assert!(json["data"].is_array());
    // Group chat should have at least one message (from creation)
    let msgs = json["data"].as_array().unwrap();
    assert!(!msgs.is_empty(), "group chat should have messages");
}
}

e2e_test! {
fn e2e_group_chat_icon_get() {
    let guid = &CONFIG.group_chat;

    // Set a known icon first so the GET has a deterministic outcome
    let form = reqwest::blocking::multipart::Form::new().part(
        "icon",
        reqwest::blocking::multipart::Part::bytes(test_png())
            .file_name("e2e-icon-get-test.png")
            .mime_str("image/png")
            .unwrap(),
    );
    let resp = client()
        .post(url(&format!("chat/{guid}/icon")))
        .multipart(form)
        .send()
        .expect("POST icon failed");
    assert_eq!(resp.status().as_u16(), 200, "set icon failed");
    std::thread::sleep(std::time::Duration::from_secs(3));

    // Now GET the icon — should return 200 with binary image data
    let resp = get_raw(&format!("chat/{guid}/icon"));
    assert_eq!(resp.status().as_u16(), 200, "icon GET should return 200 after setting icon");
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("image/"),
        "icon content-type should be an image, got: {ct}"
    );
}
}

e2e_test! {
fn e2e_group_chat_share_contact_status() {
    let guid = &CONFIG.group_chat;
    let json = get_json(&format!("chat/{guid}/share/contact/status"));
    // data is a boolean
    assert!(
        json["data"].is_boolean(),
        "share contact status data should be boolean"
    );
}
}

e2e_test! {
fn e2e_group_chat_rename() {
    let guid = &CONFIG.group_chat;
    CONFIG.webhook_receiver.drain();

    // Get current display name
    let before = get_json(&format!("chat/{guid}"));
    let original_name = before["data"]["displayName"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // Rename to test name
    let test_name = format!("E2E Test {}", run_id());
    let (status, json) = put_json(
        &format!("chat/{guid}"),
        &json!({"displayName": &test_name}),
    );
    assert_eq!(status, 200, "rename failed: {json}");

    // Verify webhook delivery for group name change
    let wh = CONFIG.webhook_receiver.wait_for_event(
        "group-name-change",
        std::time::Duration::from_secs(10),
    );
    assert_eq!(wh["type"], "group-name-change");

    std::thread::sleep(std::time::Duration::from_secs(3));

    // Verify rename took effect
    let after = get_json(&format!("chat/{guid}"));
    assert_eq!(
        after["data"]["displayName"], test_name,
        "displayName should be updated"
    );

    // Restore original name
    let restore_name = if original_name.is_empty() {
        "E2E Group"
    } else {
        &original_name
    };
    let (status, _) = put_json(
        &format!("chat/{guid}"),
        &json!({"displayName": restore_name}),
    );
    assert_eq!(status, 200, "rename restore failed");

    std::thread::sleep(std::time::Duration::from_secs(2));
}
}

e2e_test! {
fn e2e_group_chat_icon_set_remove() {
    let guid = &CONFIG.group_chat;

    // Set icon (1x1 PNG)
    let form = reqwest::blocking::multipart::Form::new().part(
        "icon",
        reqwest::blocking::multipart::Part::bytes(test_png())
            .file_name("e2e-icon.png")
            .mime_str("image/png")
            .unwrap(),
    );
    let resp = client()
        .post(url(&format!("chat/{guid}/icon")))
        .multipart(form)
        .send()
        .expect("POST icon failed");
    let status = resp.status().as_u16();
    assert_eq!(status, 200, "set icon failed");

    std::thread::sleep(std::time::Duration::from_secs(3));

    // Remove icon
    let resp = client()
        .delete(url(&format!("chat/{guid}/icon")))
        .send()
        .expect("DELETE icon failed");
    let status = resp.status().as_u16();
    assert_eq!(status, 200, "remove icon failed");

    std::thread::sleep(std::time::Duration::from_secs(2));
}
}

e2e_test! {
fn e2e_group_chat_send_text() {
    let id = run_id();
    CONFIG.webhook_receiver.drain();

    let body = json!({
        "chatGuid": CONFIG.group_chat,
        "message": format!("E2E group test #{id}"),
        "method": "private-api",
        "tempGuid": format!("temp-group-{id}"),
    });
    let (status, json) = post_json("message/text", &body);
    assert_eq!(status, 200, "group send failed: {json}");
    assert_eq!(json["data"]["isFromMe"], true);
    assert!(json["data"]["guid"].is_string());

    // Verify webhook delivery for group message
    let guid = json["data"]["guid"].as_str().unwrap();
    let wh = CONFIG.webhook_receiver.wait_for_event_with_guid(
        "new-message",
        guid,
        std::time::Duration::from_secs(10),
    );
    assert_eq!(wh["type"], "new-message");
    assert_eq!(wh["data"]["guid"], guid);
}
}

e2e_test! {
fn e2e_group_chat_delete_message() {
    let id = run_id();

    // Send a test message to the group
    let body = json!({
        "chatGuid": CONFIG.group_chat,
        "message": format!("To be deleted #{id}"),
        "method": "private-api",
    });
    let (status, send_json) = post_json("message/text", &body);
    assert_eq!(status, 200, "send for delete test failed: {send_json}");
    let msg_guid = send_json["data"]["guid"].as_str().unwrap().to_string();

    std::thread::sleep(std::time::Duration::from_secs(2));

    // Unsend first so other participants don't see test artifacts
    let (unsend_status, unsend_json) = post_json(&format!("message/{msg_guid}/unsend"), &json!({}));
    assert_eq!(unsend_status, 200, "unsend before delete failed: {unsend_json}");

    std::thread::sleep(std::time::Duration::from_secs(2));

    // Delete the message from local chat.db
    let resp = client()
        .delete(url(&format!(
            "chat/{}/{}",
            CONFIG.group_chat, msg_guid
        )))
        .send()
        .expect("DELETE message failed");
    let status = resp.status().as_u16();
    assert_eq!(status, 200, "delete message failed");
}
}

// ===========================================================================
// Auth edge cases
// ===========================================================================

e2e_test! {
fn e2e_auth_wrong_password() {
    let base = &CONFIG.base;
    let resp = reqwest::blocking::get(format!("{base}/ping?password=WRONG"))
        .expect("HTTP GET failed");
    assert_eq!(resp.status(), 401, "wrong password should return 401");
}
}

// ===========================================================================
// Not-found 404 tests (nonexistent GUIDs)
// ===========================================================================

e2e_test! {
fn e2e_message_find_nonexistent() {
    let resp = get_raw("message/nonexistent-guid-12345");
    assert_eq!(resp.status(), 404);
    let json: Value = resp.json().unwrap();
    assert!(
        error_detail(&json).contains("Message does not exist"),
        "expected 'Message does not exist' error: {json}"
    );
}
}

e2e_test! {
fn e2e_chat_find_nonexistent() {
    let resp = get_raw("chat/nonexistent-guid-12345");
    assert_eq!(resp.status(), 404);
    let json: Value = resp.json().unwrap();
    assert!(
        error_detail(&json).contains("Chat does not exist"),
        "expected 'Chat does not exist' error: {json}"
    );
}
}

e2e_test! {
fn e2e_chat_messages_nonexistent() {
    let resp = get_raw("chat/nonexistent-guid-12345/message");
    assert_eq!(resp.status(), 404);
    let json: Value = resp.json().unwrap();
    assert!(
        error_detail(&json).contains("Chat does not exist"),
        "expected 'Chat does not exist' error: {json}"
    );
}
}

e2e_test! {
fn e2e_chat_delete_nonexistent() {
    let (status, json) = delete_no_body("chat/nonexistent-guid-12345");
    assert_eq!(status, 404);
    assert!(
        error_detail(&json).contains("Chat not found"),
        "expected 'Chat not found' error: {json}"
    );
}
}

e2e_test! {
fn e2e_handle_find_nonexistent() {
    let resp = get_raw("handle/nobody-99999@nowhere.invalid");
    assert_eq!(resp.status(), 404);
    let json: Value = resp.json().unwrap();
    assert!(
        error_detail(&json).contains("Handle not found"),
        "expected 'Handle not found' error: {json}"
    );
}
}

e2e_test! {
fn e2e_handle_focus_nonexistent() {
    let resp = get_raw("handle/nobody-99999@nowhere.invalid/focus");
    assert_eq!(resp.status(), 404);
    let json: Value = resp.json().unwrap();
    assert!(
        error_detail(&json).contains("Handle not found"),
        "expected 'Handle not found' error: {json}"
    );
}
}

e2e_test! {
fn e2e_attachment_find_nonexistent() {
    let resp = get_raw("attachment/nonexistent-att-guid-12345");
    assert_eq!(resp.status(), 404);
    let json: Value = resp.json().unwrap();
    assert!(
        error_detail(&json).contains("Attachment does not exist"),
        "expected 'Attachment does not exist' error: {json}"
    );
}
}

e2e_test! {
fn e2e_attachment_download_nonexistent() {
    let resp = get_raw("attachment/nonexistent-att-guid-12345/download");
    assert_eq!(resp.status(), 404);
    let json: Value = resp.json().unwrap();
    assert!(
        error_detail(&json).contains("Attachment does not exist"),
        "expected 'Attachment does not exist' error: {json}"
    );
}
}

// ===========================================================================
// Message validation 400 tests
// ===========================================================================

e2e_test! {
fn e2e_send_text_empty_rejected() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": "",
        "method": "private-api",
    });
    let (status, json) = post_json("message/text", &body);
    assert_eq!(status, 400, "empty message should return 400: {json}");
    assert!(
        error_detail(&json).contains("required"),
        "error should mention 'required': {json}"
    );
}
}

e2e_test! {
fn e2e_send_text_invalid_method() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": "test",
        "method": "invalid-method",
    });
    let (status, json) = post_json("message/text", &body);
    assert_eq!(status, 400, "invalid method should return 400: {json}");
    assert!(
        error_detail(&json).contains("Invalid method"),
        "error should mention 'Invalid method': {json}"
    );
}
}

e2e_test! {
fn e2e_send_text_applescript_no_tempguid() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "message": "test",
        "method": "apple-script",
    });
    let (status, json) = post_json("message/text", &body);
    assert_eq!(status, 400, "apple-script without tempGuid should return 400: {json}");
    assert!(
        error_detail(&json).contains("tempGuid"),
        "error should mention 'tempGuid': {json}"
    );
}
}

e2e_test! {
fn e2e_send_reaction_empty_reaction() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": "fake-guid",
        "reaction": "",
    });
    let (status, json) = post_json("message/react", &body);
    assert_eq!(status, 400, "empty reaction should return 400: {json}");
    assert!(
        error_detail(&json).contains("reaction is required"),
        "error should mention 'reaction is required': {json}"
    );
}
}

e2e_test! {
fn e2e_send_reaction_invalid_type() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": "fake-guid",
        "reaction": "invalid-reaction",
    });
    let (status, json) = post_json("message/react", &body);
    assert_eq!(status, 400, "invalid reaction should return 400: {json}");
    assert!(
        error_detail(&json).contains("Invalid reaction"),
        "error should mention 'Invalid reaction': {json}"
    );
}
}

e2e_test! {
fn e2e_send_reaction_empty_chatguid() {
    let body = json!({
        "chatGuid": "",
        "selectedMessageGuid": "fake-guid",
        "reaction": "love",
    });
    let (status, json) = post_json("message/react", &body);
    assert_eq!(status, 400, "empty chatGuid should return 400: {json}");
    assert!(
        error_detail(&json).contains("chatGuid"),
        "error should mention 'chatGuid': {json}"
    );
}
}

e2e_test! {
fn e2e_send_reaction_nonexistent_message() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
        "reaction": "love",
    });
    let (status, json) = post_json("message/react", &body);
    assert_eq!(status, 400, "reaction on nonexistent message should return 400: {json}");
    assert!(
        error_detail(&json).contains("does not exist"),
        "error should mention 'does not exist': {json}"
    );
}
}

e2e_test! {
fn e2e_send_reaction_empty_selected_guid() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": "",
        "reaction": "love",
    });
    let (status, json) = post_json("message/react", &body);
    assert_eq!(status, 400, "empty selectedMessageGuid should return 400: {json}");
    assert!(
        error_detail(&json).contains("selectedMessageGuid"),
        "error should mention 'selectedMessageGuid': {json}"
    );
}
}

e2e_test! {
fn e2e_send_sticker_reaction_missing_data_url() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
        "reaction": "sticker",
    });
    let (status, json) = post_json("message/react", &body);
    assert_eq!(status, 400, "sticker without data URL should return 400: {json}");
    assert!(
        error_detail(&json).contains("sticker data URL is required"),
        "error should mention 'sticker data URL is required': {json}"
    );
}
}

e2e_test! {
fn e2e_send_sticker_reaction_invalid_data_url() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
        "reaction": "sticker",
        "sticker": "not-a-data-url",
    });
    let (status, json) = post_json("message/react", &body);
    assert_eq!(status, 400, "sticker with invalid data URL should return 400: {json}");
    assert!(
        error_detail(&json).contains("data:"),
        "error should mention 'data:': {json}"
    );
}
}

e2e_test! {
fn e2e_send_sticker_reaction_invalid_base64() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
        "reaction": "sticker",
        "sticker": "data:image/png;base64,!!!not-valid-base64!!!",
    });
    let (status, json) = post_json("message/react", &body);
    assert_eq!(status, 400, "sticker with invalid base64 should return 400: {json}");
    assert!(
        error_detail(&json).contains("base64"),
        "error should mention 'base64': {json}"
    );
}
}

e2e_test! {
fn e2e_send_sticker_reaction_empty_base64() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
        "reaction": "sticker",
        "sticker": "data:image/png;base64,",
    });
    let (status, json) = post_json("message/react", &body);
    assert_eq!(status, 400, "sticker with empty base64 should return 400: {json}");
    assert!(
        error_detail(&json).contains("empty"),
        "error should mention 'empty': {json}"
    );
}
}

e2e_test! {
fn e2e_send_sticker_reaction_invalid_mime() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "selectedMessageGuid": "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
        "reaction": "sticker",
        "sticker": "data:text/plain;base64,SGVsbG8=",
    });
    let (status, json) = post_json("message/react", &body);
    assert_eq!(status, 400, "sticker with non-image MIME should return 400: {json}");
    assert!(
        error_detail(&json).contains("MIME type"),
        "error should mention 'MIME type': {json}"
    );
}
}

e2e_test! {
fn e2e_edit_message_nonexistent() {
    let body = json!({
        "editedMessage": "edited text",
        "backwardsCompatibilityMessage": "edited",
    });
    let (status, json) = post_json("message/fake-guid-12345/edit", &body);
    assert_eq!(status, 400, "editing nonexistent message should return 400: {json}");
    assert!(
        error_detail(&json).contains("does not exist"),
        "error should mention 'does not exist': {json}"
    );
}
}

e2e_test! {
fn e2e_edit_message_empty_body() {
    let body = json!({
        "editedMessage": "",
        "backwardsCompatibilityMessage": "compat",
    });
    let (status, json) = post_json("message/fake-guid-12345/edit", &body);
    assert_eq!(status, 400, "empty editedMessage should return 400: {json}");
    assert!(
        error_detail(&json).contains("editedMessage is required"),
        "error should mention 'editedMessage is required': {json}"
    );
}
}

e2e_test! {
fn e2e_unsend_message_nonexistent() {
    let (status, json) = post_json("message/fake-guid-12345/unsend", &json!({}));
    assert_eq!(status, 400, "unsending nonexistent message should return 400: {json}");
    assert!(
        error_detail(&json).contains("does not exist"),
        "error should mention 'does not exist': {json}"
    );
}
}

e2e_test! {
fn e2e_notify_message_nonexistent() {
    let (status, json) = post_json("message/fake-guid-12345/notify", &json!({}));
    assert_eq!(status, 400, "notifying nonexistent message should return 400: {json}");
    assert!(
        error_detail(&json).contains("does not exist"),
        "error should mention 'does not exist': {json}"
    );
}
}

e2e_test! {
fn e2e_send_multipart_empty_parts() {
    let body = json!({
        "chatGuid": CONFIG.self_chat,
        "parts": [],
    });
    let (status, json) = post_json("message/multipart", &body);
    assert_eq!(status, 400, "empty parts should return 400: {json}");
    assert!(
        error_detail(&json).contains("parts"),
        "error should mention 'parts': {json}"
    );
}
}

// ===========================================================================
// Chat validation 400/500 tests
// ===========================================================================

e2e_test! {
fn e2e_chat_new_empty_addresses() {
    let body = json!({
        "addresses": [],
        "message": "test",
        "method": "private-api",
    });
    let (status, json) = post_json("chat/new", &body);
    assert_eq!(status, 400, "empty addresses should return 400: {json}");
    assert!(
        error_detail(&json).contains("addresses"),
        "error should mention 'addresses': {json}"
    );
}
}

e2e_test! {
fn e2e_chat_rename_1to1_rejected() {
    let self_chat = &CONFIG.self_chat;
    let (status, json) = put_json(
        &format!("chat/{self_chat}"),
        &json!({"displayName": "Fail"}),
    );
    assert_eq!(status, 500, "renaming 1:1 chat should return 500: {json}");
    assert!(
        error_detail(&json).contains("non-group"),
        "error should mention 'non-group': {json}"
    );
}
}

e2e_test! {
fn e2e_chat_rename_no_displayname() {
    let group = &CONFIG.group_chat;
    let (status, json) = put_json(&format!("chat/{group}"), &json!({}));
    assert_eq!(status, 200, "no displayName should return 200: {json}");
    assert!(
        json["message"].as_str().unwrap_or("").contains("not updated"),
        "message should contain 'not updated': {json}"
    );
}
}

e2e_test! {
fn e2e_chat_icon_set_1to1_rejected() {
    let self_chat = &CONFIG.self_chat;
    let form = reqwest::blocking::multipart::Form::new().part(
        "icon",
        reqwest::blocking::multipart::Part::bytes(test_png())
            .file_name("e2e-icon-1to1.png")
            .mime_str("image/png")
            .unwrap(),
    );
    let resp = client()
        .post(url(&format!("chat/{self_chat}/icon")))
        .multipart(form)
        .send()
        .expect("POST icon failed");
    let status = resp.status().as_u16();
    let json: Value = resp.json().unwrap();
    assert_eq!(status, 500, "setting icon on 1:1 should return 500: {json}");
    assert!(
        error_detail(&json).contains("not a group"),
        "error should mention 'not a group': {json}"
    );
}
}

e2e_test! {
fn e2e_chat_participant_add_empty_address() {
    let group = &CONFIG.group_chat;
    let (status, json) = post_json(
        &format!("chat/{group}/participant/add"),
        &json!({"address": ""}),
    );
    assert_eq!(status, 400, "empty address should return 400: {json}");
    assert!(
        error_detail(&json).contains("address"),
        "error should mention 'address': {json}"
    );
}
}

// ===========================================================================
// Attachment & iCloud validation
// ===========================================================================

e2e_test! {
fn e2e_attachment_force_download_nonexistent() {
    let resp = get_raw("attachment/nonexistent-att-guid-12345/download/force");
    assert_eq!(
        resp.status().as_u16(),
        400,
        "force download of nonexistent attachment should return 400"
    );
    let json: Value = resp.json().unwrap();
    assert!(
        error_detail(&json).contains("does not exist"),
        "error should mention 'does not exist': {json}"
    );
}
}

e2e_test! {
fn e2e_icloud_contact_no_address() {
    let resp = get_raw("icloud/contact");
    assert_eq!(
        resp.status().as_u16(),
        400,
        "icloud contact without address should return 400"
    );
    let json: Value = resp.json().unwrap();
    assert!(
        error_detail(&json).contains("address"),
        "error should mention 'address': {json}"
    );
}
}

// ===========================================================================
// Query parameter variations
// ===========================================================================

e2e_test! {
fn e2e_message_query_asc() {
    let body = json!({
        "limit": 5,
        "sort": "ASC",
    });
    let (status, json) = post_json("message/query", &body);
    assert_eq!(status, 200);
    let data = json["data"].as_array().expect("data should be an array");
    assert!(data.len() >= 2, "need at least 2 messages for sort check");
    let d0 = data[0]["dateCreated"].as_u64().unwrap_or(0);
    let d1 = data[1]["dateCreated"].as_u64().unwrap_or(0);
    assert!(d0 <= d1, "messages should be in ASC order: {d0} > {d1}");
}
}

e2e_test! {
fn e2e_message_query_pagination() {
    // Page 1: first 3 messages
    let body1 = json!({ "limit": 3, "offset": 0, "sort": "DESC" });
    let (status1, json1) = post_json("message/query", &body1);
    assert_eq!(status1, 200);
    let page1 = json1["data"].as_array().expect("page1 should be array");
    assert_eq!(page1.len(), 3, "page1 should have 3 messages");

    // Page 2: next 3 messages
    let body2 = json!({ "limit": 3, "offset": 3, "sort": "DESC" });
    let (status2, json2) = post_json("message/query", &body2);
    assert_eq!(status2, 200);
    let page2 = json2["data"].as_array().expect("page2 should be array");
    assert_eq!(page2.len(), 3, "page2 should have 3 messages");
    assert_eq!(json2["metadata"]["offset"], 3, "page2 metadata.offset should be 3");

    // No overlap between pages
    let guids1: Vec<&str> = page1.iter().filter_map(|m| m["guid"].as_str()).collect();
    let guids2: Vec<&str> = page2.iter().filter_map(|m| m["guid"].as_str()).collect();
    for g in &guids2 {
        assert!(
            !guids1.contains(g),
            "page2 should not overlap with page1, found duplicate: {g}"
        );
    }
}
}

e2e_test! {
fn e2e_message_query_with_chat_participants() {
    let body = json!({
        "limit": 3,
        "sort": "DESC",
        "with": ["chat", "chat.participants"],
    });
    let (status, json) = post_json("message/query", &body);
    assert_eq!(status, 200);
    let data = json["data"].as_array().expect("data should be array");
    assert!(!data.is_empty(), "should have at least one message");

    // Find a message that has chats (most will)
    let msg_with_chats = data.iter().find(|m| {
        m["chats"].as_array().map_or(false, |c| !c.is_empty())
    });
    if let Some(msg) = msg_with_chats {
        assert!(
            msg["chats"][0]["participants"].is_array(),
            "chat should have participants array when with=[chat.participants]"
        );
    }
}
}

e2e_test! {
fn e2e_chat_query_with_participants() {
    let body = json!({
        "limit": 5,
        "with": ["participants"],
    });
    let (status, json) = post_json("chat/query", &body);
    assert_eq!(status, 200);
    let chats = json["data"].as_array().expect("data should be array");
    assert!(!chats.is_empty(), "should have at least one chat");
    for (i, chat) in chats.iter().enumerate() {
        assert!(
            chat["participants"].is_array(),
            "chat[{i}] should have participants array when with=[participants]"
        );
    }
}
}

e2e_test! {
fn e2e_chat_find_with_last_message() {
    let self_chat = &CONFIG.self_chat;
    let json = get_json(&format!("chat/{self_chat}?with=lastmessage"));
    let data = &json["data"];
    assert!(
        data["lastMessage"].is_object(),
        "should have lastMessage object: {data}"
    );
    assert!(
        data["lastMessage"]["guid"].is_string(),
        "lastMessage should have guid"
    );
    assert!(
        data["lastMessage"]["dateCreated"].is_u64(),
        "lastMessage should have dateCreated"
    );
}
}

e2e_test! {
fn e2e_chat_find_with_participants() {
    let group = &CONFIG.group_chat;
    let json = get_json(&format!("chat/{group}?with=participants"));
    let data = &json["data"];
    let participants = data["participants"]
        .as_array()
        .expect("should have participants array");
    assert!(
        participants.len() >= 2,
        "group chat should have at least 2 participants, got {}",
        participants.len()
    );
    for (i, p) in participants.iter().enumerate() {
        assert!(
            p["address"].is_string(),
            "participant[{i}] should have address"
        );
    }
}
}

e2e_test! {
fn e2e_chat_count_breakdown() {
    let json = get_json("chat/count");
    let data = &json["data"];
    assert!(data["total"].is_u64(), "should have total");
    assert!(
        data["breakdown"].is_object(),
        "should have breakdown object: {data}"
    );
    assert!(
        data["breakdown"]["iMessage"].is_u64() || data["breakdown"]["iMessage"].is_i64(),
        "breakdown should have iMessage key: {data}"
    );
}
}

e2e_test! {
fn e2e_statistics_totals_only_filter() {
    let json = get_json("server/statistics/totals?only=messages");
    let data = &json["data"];
    assert!(
        data["messages"].is_u64(),
        "data.messages should be present when only=messages"
    );
    assert!(
        data.get("handles").is_none(),
        "data.handles should be absent when only=messages: {data}"
    );
    assert!(
        data.get("chats").is_none(),
        "data.chats should be absent when only=messages: {data}"
    );
    assert!(
        data.get("attachments").is_none(),
        "data.attachments should be absent when only=messages: {data}"
    );
}
}

e2e_test! {
fn e2e_server_logs_count() {
    let json = get_json("server/logs?count=3");
    let log_text = json["data"].as_str().expect("logs data should be a string");
    let line_count = log_text.lines().count();
    assert!(
        line_count >= 1 && line_count <= 3,
        "log count should be between 1 and 3, got {line_count}"
    );
}
}

// ===========================================================================
// Previously untested routes
// ===========================================================================

e2e_test! {
fn e2e_group_chat_share_contact() {
    let guid = &CONFIG.group_chat;
    let (status, json) = post_json(&format!("chat/{guid}/share/contact"), &json!({}));
    assert_eq!(status, 200, "share contact should return 200: {json}");
    assert!(
        json["message"].as_str().unwrap_or("").contains("Shared")
            || json["message"].as_str().unwrap_or("").contains("shared"),
        "message should mention sharing: {json}"
    );
}
}

e2e_test! {
fn e2e_facetime_leave() {
    let c = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap();
    let resp = c
        .post(url("facetime/leave/00000000-0000-0000-0000-000000000000"))
        .json(&json!({}))
        .send()
        .expect("facetime leave request failed");
    let status = resp.status().as_u16();
    let json: Value = resp.json().unwrap_or_default();
    assert_eq!(status, 201, "facetime leave should return 201: {json}");
    assert_eq!(json["status"], 201);
    assert_eq!(json["message"], "No Data");
}
}

// ===========================================================================
// Webhook verification: mark read/unread dispatches events
// ===========================================================================

e2e_test! {
fn e2e_mark_read_unread_webhook() {
    let self_chat = &CONFIG.self_chat;
    let timeout = std::time::Duration::from_secs(10);

    // --- Mark unread ---
    CONFIG.webhook_receiver.drain();
    let resp = client()
        .post(url(&format!("chat/{self_chat}/unread")))
        .send()
        .expect("POST unread failed");
    assert_eq!(resp.status(), 200, "mark unread should return 200");

    let wh = CONFIG
        .webhook_receiver
        .wait_for_event("chat-read-status-changed", timeout);
    assert_eq!(wh["type"], "chat-read-status-changed");
    assert_eq!(
        wh["data"]["read"], false,
        "mark_unread webhook should have read=false: {wh}"
    );

    // --- Mark read ---
    CONFIG.webhook_receiver.drain();
    let resp = client()
        .post(url(&format!("chat/{self_chat}/read")))
        .send()
        .expect("POST read failed");
    assert_eq!(resp.status(), 200, "mark read should return 200");

    let wh = CONFIG
        .webhook_receiver
        .wait_for_event("chat-read-status-changed", timeout);
    assert_eq!(wh["type"], "chat-read-status-changed");
    assert_eq!(
        wh["data"]["read"], true,
        "mark_read webhook should have read=true: {wh}"
    );
}
}

// ===========================================================================
// Group lifecycle tests (require 3 peers)
// ===========================================================================

// Full group lifecycle: create (4 members) → verify → remove participant → verify →
// add back → verify → leave (4→3, still valid group) → best-effort delete.
//
// Requires 3 peers (4 members with self) because iMessage won't let you:
// - remove a member if it would drop below 3
// - leave a group if it would drop below 3
e2e_test! {
fn e2e_group_lifecycle() {
    if CONFIG.peers.len() < 3 {
        eprintln!("Skipping lifecycle test: need 3 peers, have {}", CONFIG.peers.len());
        return;
    }

    // Client with longer timeout — participant/leave ops poll DB up to 30s
    let tc = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()
        .unwrap();

    // --- Create group with all 3 peers (4 members total with self) ---
    let create_body = json!({
        "addresses": [&CONFIG.peers[0], &CONFIG.peers[1], &CONFIG.peers[2]],
        "message": "E2E lifecycle test group",
        "method": "private-api",
    });
    let resp = tc
        .post(url("chat/new"))
        .json(&create_body)
        .send()
        .expect("Failed to create lifecycle group");
    let status = resp.status().as_u16();
    let json: Value = resp.json().expect("Invalid JSON from /chat/new");
    assert_eq!(status, 200, "create lifecycle group failed: {json}");
    let guid = json["data"]["guid"]
        .as_str()
        .expect("created group missing guid")
        .to_string();
    eprintln!("  Lifecycle group created: {guid}");

    std::thread::sleep(std::time::Duration::from_secs(3));

    // --- Verify: style=43, participants >= 3 ---
    let chat_json = get_json(&format!("chat/{guid}?with=participants"));
    assert_eq!(chat_json["data"]["style"], 43, "should be a group chat");
    let participants = chat_json["data"]["participants"]
        .as_array()
        .expect("should have participants");
    assert!(
        participants.len() >= 3,
        "lifecycle group should have >= 3 participants, got {}",
        participants.len()
    );

    // --- Remove peers[2] (4→3) ---
    let remove_body = json!({"address": &CONFIG.peers[2]});
    let resp = tc
        .post(url(&format!("chat/{guid}/participant/remove")))
        .json(&remove_body)
        .send()
        .expect("remove participant failed");
    let status = resp.status().as_u16();
    let remove_json: Value = resp.json().expect("Invalid JSON from participant/remove");
    assert_eq!(status, 200, "remove participant failed: {remove_json}");

    std::thread::sleep(std::time::Duration::from_secs(3));

    // Verify peers[2] is absent
    let after_remove = get_json(&format!("chat/{guid}?with=participants"));
    let addrs_after_remove: Vec<String> = after_remove["data"]["participants"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|p| p["address"].as_str().map(|s| s.to_lowercase()))
        .collect();
    assert!(
        !addrs_after_remove.contains(&CONFIG.peers[2].to_lowercase()),
        "peers[2] should be absent after removal, participants: {addrs_after_remove:?}"
    );

    // --- Add peers[2] back (3→4) ---
    let add_body = json!({"address": &CONFIG.peers[2]});
    let resp = tc
        .post(url(&format!("chat/{guid}/participant/add")))
        .json(&add_body)
        .send()
        .expect("add participant failed");
    let status = resp.status().as_u16();
    let add_json: Value = resp.json().expect("Invalid JSON from participant/add");
    assert_eq!(status, 200, "add participant failed: {add_json}");

    std::thread::sleep(std::time::Duration::from_secs(3));

    // Verify peers[2] is present again
    let after_add = get_json(&format!("chat/{guid}?with=participants"));
    let addrs_after_add: Vec<String> = after_add["data"]["participants"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|p| p["address"].as_str().map(|s| s.to_lowercase()))
        .collect();
    assert!(
        addrs_after_add.contains(&CONFIG.peers[2].to_lowercase()),
        "peers[2] should be present after re-add, participants: {addrs_after_add:?}"
    );

    // --- Leave group (4→3, still valid) ---
    let resp = tc
        .post(url(&format!("chat/{guid}/leave")))
        .json(&json!({}))
        .send()
        .expect("leave group failed");
    let status = resp.status().as_u16();
    let leave_json: Value = resp.json().expect("Invalid JSON from /chat/leave");
    assert_eq!(status, 200, "leave group failed: {leave_json}");

    // Best-effort cleanup: delete the group
    let _ = delete_no_body(&format!("chat/{guid}"));
}
}
