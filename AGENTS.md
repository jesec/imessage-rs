# AGENTS.md

This file provides guidance to AI coding agents when working with code in this repository.

## Build & Test Commands

```bash
cargo build                    # build workspace (also compiles Swift dylib via build.rs)
cargo test                     # run all 175 unit tests
cargo test -p imessage-core    # test a single crate
cargo test config::tests::merge_cli_overrides_yaml  # run a single test by name
cargo clippy --workspace       # lint
cargo fmt --all -- --check     # check formatting

# Swift dylib (auto-built by cargo, but can be built manually)
make -C crates/imessage-private-api/swift
make -C crates/imessage-private-api/swift check
cd crates/imessage-private-api/swift && swift test   # 26 Swift tests

# E2E tests (auto-compiles and spawns server; requires real iMessage accounts)
E2E_PEER_ADDRS=a@icloud.com,b@icloud.com,c@icloud.com \
  cargo test --test e2e -- --ignored

# Live DB tests (require Full Disk Access to ~/Library/Messages/chat.db)
cargo test -p imessage-db --test live_chatdb -- --ignored
```

## Architecture

**Rust workspace with 8 library crates + root binary** (`src/main.rs`).

### Crate dependency flow
```
imessage-core (config, dates, phone normalization, typedstream decoder)
  ├── imessage-db (rusqlite read-only layer for chat.db)
  │     ├── imessage-serializers (entity → JSON)
  │     └── imessage-watcher (file-system watcher + DB pollers → broadcast events)
  ├── imessage-apple (AppleScript generation + execution)
  ├── imessage-private-api (TCP service + embedded Swift dylib)
  ├── imessage-webhooks (HTTP POST dispatch to registered URLs)
  └── imessage-http (axum 0.8 server, 66 routes, middleware)
```

### Two send paths
- **AppleScript** (`imessage-apple`): basic text/attachment sends via `osascript`
- **Private API** (`imessage-private-api`): full-featured — reactions, edits, unsends, typing, effects, formatting, FaceTime, FindMy devices. Works by injecting a Swift dylib into Messages.app/FaceTime.app/FindMy.app via `DYLD_INSERT_LIBRARIES`.

### Private API protocol
Newline-delimited JSON over TCP on `127.0.0.1` (port `45670 + uid - 501`).

**Readiness phases**: Disconnected → Connected (TCP + ping) → Ready (IMCore initialized).
Per-process readiness tracked via `AtomicBool` (`messages_ready` / `facetime_ready` / `findmy_ready`).
Routes gate on readiness via `require_private_api()` / `require_facetime_private_api()` / `require_findmy_private_api()`.

**Actions** (Rust→Swift): serialized with `transactionId`, throttled by `Semaphore(1)` with 200ms release delay. Transactions timeout after 120 seconds.

**Events** (Swift→Rust): broadcast to subscribers via `tokio::sync::broadcast`. `RawEvent` uses `#[serde(flatten)]` to capture extra fields — `extract_data()` prefers explicit `data` field, falls back to flattened extras.

### Event pipeline
```
chat.db changes → notify file watcher → poll_messages/poll_chat_reads → WatcherEvent
Private API events → broadcast channel → typing/FaceTime/FindMy/aliases
Both sources → WebhookService → HTTP POST to registered URLs (dedup via EventCache, 1-hour TTL)
```

### HTTP layer
All routes under `/api/v1/`. Auth via `?password=` query param. Response envelope: `{ status, message, data, metadata }` or `{ status, message, error: { type, message } }`. Pretty JSON via `?pretty` param. `AppState` holds shared `MessageRepository` (parking_lot::Mutex), optional `PrivateApiService`, webhook service, and various caches.

## Key Conventions

- **macOS Sequoia (15) minimum** — startup guard enforces this. Only `is_min_tahoe()` varies at runtime.
- **macOS Tahoe chat GUID prefix**: `any;` instead of `iMessage;`/`SMS;` — `normalize_chat_guid()` in validators.rs handles this.
- **Swift dylib must be arm64e** — Messages.app runs as arm64e; DYLD_INSERT_LIBRARIES requires arch match.
- **Dylib is embedded at compile time** via `include_bytes!()`, written to disk at runtime.
- **parking_lot::Mutex** for `MessageRepository` (rusqlite Connection is not Send+Sync). tokio::sync::Mutex for async operations.
- **serde_json `preserve_order`** is enabled workspace-wide — JSON keys maintain insertion order.
- **Watcher initial seed**: first poll uses 60s lookback to populate caches without emitting events.
- **Clean shutdown kills Messages.app, FaceTime.app, and FindMy.app** to remove injected dylibs.

## E2E Test Details

- Auto-spawns server binary; panics if port 1234 already in use
- `E2E_PEER_ADDRS`: 3 peers enables group lifecycle tests; 2 is minimum
- `E2E_BASE`: set to skip auto-spawn and use an external server
- Built-in `WebhookReceiver` captures POSTs for assertion
- Tests send **real iMessages** to real Apple IDs
- Do NOT add tests that restart Messages.app — kills the Private API dylib connection

## Data locations

- Config: `~/Library/Application Support/imessage-rs/config.yml`
- Data dir: `~/Library/Application Support/imessage-rs/`
- PID file: `~/Library/Application Support/imessage-rs/.imessage-rs.pid`
- Dylib: `~/Library/Application Support/imessage-rs/private-api/imessage-helper.dylib`
- Logs: `~/Library/Application Support/imessage-rs/logs/main.log`
