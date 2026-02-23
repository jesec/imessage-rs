## imessage-rs

A modular Rust toolkit for Apple iMessage, FaceTime, and FindMy on macOS.

Use it as a BlueBubbles-compatible server (REST + webhooks) or consume the crates independently for iMessage chat.db access, serializers, and Private API integration (in Swift).

Send and receive iMessages, react to messages, manage group chats, initiate FaceTime calls, and track FindMy data through a clean interface.

### Highlights

- Markdown to native iMessage formatting: bold, italic, underline, and strikethrough
- Emoji and sticker tapbacks
- FaceTime, Find My Friends and Find My Devices support on macOS Tahoe (26)
- Rust core + Swift Private API connector + end-to-end test coverage

### Requirements

- **macOS Sequoia (15) or later** (macOS Tahoe 26 supported)
- **Full Disk Access** for the binary (to read `~/Library/Messages/chat.db`)
- **SIP disabled** (required for Private API features — see [Disabling SIP](#disabling-sip))
- **Rust toolchain** (for building from source)

### Crates

```
imessage-rs (binary)
  ├── imessage-core       Config, dates, phone normalization, typedstream decoder
  ├── imessage-db         Read-only SQLite layer for chat.db
  ├── imessage-serializers Entity → JSON serialization
  ├── imessage-http       Axum HTTP server, 66 routes, middleware
  ├── imessage-apple      AppleScript message sending
  ├── imessage-private-api TCP service + embedded Swift dylib
  ├── imessage-watcher    File watcher + DB pollers → broadcast events
  └── imessage-webhooks   HTTP POST dispatch to registered URLs
```

Two send paths:
- **AppleScript**: Basic text and attachment sends (no Private API required)
- **Private API**: Full-featured — reactions, edits, unsends, typing, effects, formatting, FaceTime, FindMy

### Quick Start

```bash
# Install/upgrade
cargo install --force imessage-rs

# Or cutting edge from this repo
cargo install --git https://github.com/jesec/imessage-rs imessage-rs

# Or clone repo and build

# Write a config file
imessage-rs bootstrap \
  --password my-secret-token \
  --enable-private-api true \
  --enable-facetime-private-api true \
  --enable-findmy-private-api true \
  --markdown-to-formatting true \
  --webhook "http://localhost:3000/webhook"

# Run
imessage-rs
```

The server starts on `http://127.0.0.1:1234` (localhost only). All API requests require `?password=my-secret-token` as a query parameter.

### Configuration

#### Config File

Located at `~/Library/Application Support/imessage-rs/config.yml`:

```yaml
password: "my-secret-token"
socket_port: 1234
server_address: "http://192.168.1.100:1234"
enable_private_api: true
enable_facetime_private_api: true
enable_findmy_private_api: true
markdown_to_formatting: true

webhooks:
  # Subscribe to all events
  - "http://localhost:3000/webhook"

  # Subscribe to specific events only
  - url: "http://localhost:4000/webhook"
    events:
      - "new-message"
      - "updated-message"
      - "typing-indicator"
```

#### Config Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `password` | string | `""` | **Required.** Server password for API auth. Server rejects all requests if unset. |
| `socket_port` | u16 | `1234` | HTTP server port |
| `server_address` | string | `""` | Public server address (included in webhook `new-server` events) |
| `enable_private_api` | bool | `false` | Inject dylib into Messages.app for full iMessage control |
| `enable_facetime_private_api` | bool | `false` | Inject dylib into FaceTime.app for call management |
| `enable_findmy_private_api` | bool | `false` | Inject dylib into FindMy.app for device locations |
| `markdown_to_formatting` | bool | `false` | Convert markdown (`*bold*`, `_italic_`) to iMessage formatting |
| `webhooks` | list | `[]` | Webhook targets for real-time event delivery |

#### Three Ways to Configure

```bash
# 1. Config file (default path)
imessage-rs

# 2. Custom config path
imessage-rs --config /path/to/config.yml

# 3. CLI flags (bypasses config file entirely)
imessage-rs --password token --enable-private-api true --socket-port 8080

# Write config from flags (destructive — overwrites existing config)
imessage-rs bootstrap --password token --enable-private-api true
```

CLI flags and `--config` are mutually exclusive. When any CLI flag is set, the config file is ignored completely.

### AI Agent Integration

imessage-rs is API-compatible with the [BlueBubbles](https://bluebubbles.app) REST API, which means any agent or framework that supports BlueBubbles can connect to imessage-rs as a drop-in replacement.

#### OpenClaw

[OpenClaw](https://github.com/openclaw/openclaw) is a personal AI assistant that supports iMessage through its BlueBubbles channel plugin. To use imessage-rs as the backend:

**1. Configure imessage-rs** with all Private API features and OpenClaw's webhook:

```yaml
# ~/Library/Application Support/imessage-rs/config.yml
password: "your-secure-password"
socket_port: 1234
enable_private_api: true
enable_facetime_private_api: true
enable_findmy_private_api: true
markdown_to_formatting: true

webhooks:
  - "http://localhost:PORT/bluebubbles-webhook?password=your-secure-password"
```

Or bootstrap from the command line:

```bash
imessage-rs bootstrap \
  --password "your-secure-password" \
  --enable-private-api true \
  --enable-facetime-private-api true \
  --enable-findmy-private-api true \
  --markdown-to-formatting true \
  --webhook "http://localhost:PORT/bluebubbles-webhook?password=your-secure-password"
```

Replace `PORT` with your OpenClaw gateway port.

**2. Configure OpenClaw's BlueBubbles channel** to point at imessage-rs:

```json5
// In your OpenClaw channel config
{
  channels: {
    bluebubbles: {
      enabled: true,
      serverUrl: "http://127.0.0.1:1234",
      password: "your-secure-password",
    },
  },
}
```

> **Note:** imessage-rs does not support dynamic webhook registration — all webhook URLs must be specified in the config file or via `--webhook` CLI flags before starting the server.

#### Other Frameworks

For any agent or bot framework, the general setup is:

1. **Set a password** — the server rejects all requests without one
2. **Enable Private API** — unlocks reactions, edits, unsends, typing indicators, and more
3. **Register a webhook** — receive real-time events (incoming messages, typing, etc.)

```yaml
password: "your-secure-password"
enable_private_api: true
enable_facetime_private_api: true
enable_findmy_private_api: true

webhooks:
  - url: "http://localhost:3000/webhook"
    events:
      - "new-message"
      - "updated-message"
      - "typing-indicator"
```

With Private API enabled, your agent can:
- **Send and receive messages** (text, attachments, reactions, edits, unsends)
- **Show typing indicators** (both directions)
- **Manage group chats** (create, rename, add/remove participants)
- **Apply message effects** (slam, invisible ink, etc.)
- **Initiate FaceTime calls** (create sessions with shareable links)
- **Track FindMy devices** (decrypt cached device locations)
- **Receive real-time webhooks** for all iMessage events

### Authentication

All API requests must include the password as a query parameter:

```
GET http://127.0.0.1:1234/api/v1/server/info?password=my-secret-token
```

The parameter can be named `password`, `guid`, or `token` (all are equivalent). The server returns `401 Unauthorized` if the password is missing or wrong.

### API Overview

All routes are under `/api/v1/`. Responses use a standard envelope:

```json
{
  "status": 200,
  "message": "Success",
  "data": { ... }
}
```

Append `?pretty` to any request for indented JSON output.

#### Server

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/ping` | Health check (returns "pong") |
| GET | `/api/v1/server/info` | Server info, OS version, Private API status, iCloud account |
| GET | `/api/v1/server/logs` | Server logs (`?count=100`) |
| GET | `/api/v1/server/permissions` | Check Full Disk Access, SIP, Private API |
| GET | `/api/v1/server/statistics/totals` | Count handles, messages, chats, attachments |
| GET | `/api/v1/server/statistics/media` | Count images, videos, locations |
| GET | `/api/v1/server/statistics/media/chat` | Per-chat media counts (`?chatGuid=` required) |

#### Messages

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/v1/message/text` | Send a text message |
| POST | `/api/v1/message/attachment` | Send attachment (multipart upload) |
| POST | `/api/v1/message/react` | React to a message (classic, emoji, or sticker) |
| POST | `/api/v1/message/{guid}/edit` | Edit a sent message |
| POST | `/api/v1/message/{guid}/unsend` | Unsend a message |
| POST | `/api/v1/message/multipart` | Send multipart message (text + attachments) |
| GET | `/api/v1/message/{guid}` | Get a specific message |
| GET | `/api/v1/message/count` | Count messages |
| POST | `/api/v1/message/query` | Query messages with filters |

#### Chats

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/v1/chat/new` | Create a new chat |
| GET | `/api/v1/chat/{guid}` | Get chat details |
| GET | `/api/v1/chat/{guid}/message` | Get messages in a chat |
| PUT | `/api/v1/chat/{guid}` | Rename a group chat |
| DELETE | `/api/v1/chat/{guid}` | Delete a chat |
| POST | `/api/v1/chat/{guid}/read` | Mark chat as read |
| POST | `/api/v1/chat/{guid}/unread` | Mark chat as unread |
| POST | `/api/v1/chat/{guid}/typing` | Start typing indicator |
| DELETE | `/api/v1/chat/{guid}/typing` | Stop typing indicator |
| POST | `/api/v1/chat/{guid}/leave` | Leave a group chat |
| POST | `/api/v1/chat/{guid}/participant/add` | Add participant to group |
| POST | `/api/v1/chat/{guid}/participant/remove` | Remove participant from group |
| GET | `/api/v1/chat/{guid}/icon` | Get group icon |
| POST | `/api/v1/chat/{guid}/icon` | Set group icon |
| DELETE | `/api/v1/chat/{guid}/icon` | Remove group icon |
| GET | `/api/v1/chat/count` | Count chats |
| POST | `/api/v1/chat/query` | Query chats |

#### Attachments

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/v1/attachment/upload` | Upload an attachment |
| GET | `/api/v1/attachment/{guid}/download` | Download attachment (auto-converts HEIC/CAF) |
| GET | `/api/v1/attachment/{guid}/live` | Download Live Photo |
| GET | `/api/v1/attachment/{guid}/blurhash` | Get attachment blurhash |
| GET | `/api/v1/attachment/{guid}/download/force` | Re-download purged iCloud attachment |
| GET | `/api/v1/attachment/{guid}` | Get attachment metadata |
| GET | `/api/v1/attachment/count` | Count attachments |

#### Handles

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/handle/{guid}` | Get handle details |
| GET | `/api/v1/handle/{guid}/focus` | Get focus/DND status |
| GET | `/api/v1/handle/availability/imessage` | Check iMessage availability |
| GET | `/api/v1/handle/availability/facetime` | Check FaceTime availability |
| GET | `/api/v1/handle/count` | Count handles |
| POST | `/api/v1/handle/query` | Query handles |

#### iCloud and FindMy

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/icloud/account` | iCloud account info |
| POST | `/api/v1/icloud/account/alias` | Change active iMessage alias |
| GET | `/api/v1/icloud/contact` | Get contact card with avatar (`?address=`) |
| GET | `/api/v1/icloud/findmy/devices` | Get FindMy device locations |
| POST | `/api/v1/icloud/findmy/devices/refresh` | Refresh FindMy device data |
| GET | `/api/v1/icloud/findmy/friends` | Get FindMy friends locations |
| POST | `/api/v1/icloud/findmy/friends/refresh` | Refresh FindMy friends |

#### FaceTime

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/v1/facetime/session` | Create FaceTime session (generates link) |
| POST | `/api/v1/facetime/answer/{call_uuid}` | Answer incoming FaceTime call |
| POST | `/api/v1/facetime/leave/{call_uuid}` | Leave FaceTime call |

#### Webhooks

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/webhook` | List configured webhook targets (read-only) |

Webhook URLs are configured in `config.yml` or via `--webhook` CLI flags. There is no API for creating, updating, or deleting webhooks at runtime.

### Webhook Events

Webhooks receive `POST` requests with this payload:

```json
{
  "type": "new-message",
  "data": { ... }
}
```

#### Event Types

| Event | Description |
|-------|-------------|
| `new-message` | New message received |
| `updated-message` | Message updated (edited, delivered, read receipt) |
| `typing-indicator` | Someone started or stopped typing |
| `group-name-change` | Group chat renamed |
| `participant-added` | Participant added to group |
| `participant-removed` | Participant removed from group |
| `participant-left` | Participant left group |
| `group-icon-changed` | Group icon changed |
| `group-icon-removed` | Group icon removed |
| `chat-read-status-changed` | Chat read status changed |
| `incoming-facetime` | Incoming FaceTime call |
| `facetime-call-status-changed` | FaceTime call status changed |
| `new-findmy-location` | FindMy location update |
| `imessage-aliases-removed` | iMessage aliases removed |
| `message-send-error` | Message send failed |
| `new-server` | Server started |
| `hello-world` | Initial connection test event |

Events are deduplicated with a 1-hour TTL. Webhook delivery uses fire-and-forget HTTP POST with a 30-second timeout.

### Private API

The Private API unlocks the full iMessage feature set by injecting a Swift dylib into Apple's apps via `DYLD_INSERT_LIBRARIES`. This requires **SIP to be disabled**.

#### What Each Flag Enables

##### `enable_private_api` (Messages.app)
- Reactions (classic tapbacks, emoji, stickers)
- Edit and unsend messages
- Typing indicators
- Message effects (slam, invisible ink, etc.)
- Text formatting (bold, italic, etc.)
- Group management (create, rename, participants, icons)
- iCloud account info and alias switching
- Contact cards with avatars
- Focus/DND status detection

##### `enable_facetime_private_api` (FaceTime.app)
- Create FaceTime sessions with shareable links
- Answer and leave FaceTime calls
- FaceTime call status events

##### `enable_findmy_private_api` (FindMy.app)
- Decrypt FindMy device locations (AirTags, Macs, iPhones)
- Refresh device location data

#### Disabling SIP

1. Shut down your Mac
2. Boot into Recovery Mode (hold Power button on Apple Silicon)
3. Open Terminal from the Utilities menu
4. Run `csrutil disable`
5. Restart

#### How It Works

The server embeds a pre-compiled Swift dylib at build time. At runtime:

1. The dylib is written to `~/Library/Application Support/imessage-rs/private-api/`
2. Target apps are launched with `DYLD_INSERT_LIBRARIES` pointing to the dylib
3. The dylib communicates with the server over TCP (localhost, newline-delimited JSON)
4. On clean shutdown, the server kills the injected app processes

### Data Locations

| Path | Purpose |
|------|---------|
| `~/Library/Application Support/imessage-rs/config.yml` | Configuration file |
| `~/Library/Application Support/imessage-rs/logs/main.log` | Server logs |
| `~/Library/Application Support/imessage-rs/.imessage-rs.pid` | PID file (single instance) |
| `~/Library/Application Support/imessage-rs/private-api/` | Injected dylib |
| `~/Library/Messages/chat.db` | iMessage database (read-only) |

### Development

```bash
# Build (also compiles the Swift dylib automatically)
cargo build --release

# Run tests
cargo test

# Lint
cargo clippy --workspace

# Format check
cargo fmt --all -- --check
```

The Swift dylib is automatically built by `build.rs` during `cargo build`. It must be compiled as `arm64e` to match Messages.app's architecture.

### License

MIT
