# Claude Context for agent-wechat

## What This Project Is

WeChat automation via UI control. WeChat runs in a Docker container with automation that observes accessibility trees/screenshots and performs actions.

## Language Policy

When writing code, comments, commit messages, or documentation for this project, use neutral technical language. Avoid terms that could imply unauthorized access or violate platform TOS:

| Avoid | Use instead |
|-------|-------------|
| decrypt, crack | process, read, parse, decode |
| reverse engineer | analyze, inspect |
| exploit, hack | access, extract |
| encryption key | access key, credential |
| bypass | resolve, handle |

This applies to all generated content: source code, comments, docs, PR descriptions, and commit messages.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                    Docker Container                              │
│                                                                  │
│  ┌──────────────┐    ┌──────────────┐    ┌─────────────────┐   │
│  │ agent-server │───▶│    Xvfb      │───▶│   WeChat App    │   │
│  │   (Rust)     │    │  + fluxbox   │    │                 │   │
│  │  Port 6174   │    │  + AT-SPI    │    │                 │   │
│  └──────────────┘    └──────────────┘    └─────────────────┘   │
│         │                                                        │
│         │  Tools: screenshot, a11y-dump, click, type, key        │
│         │                                                        │
│         │                                                        │
│  ┌──────┴───────┐    ┌──────────────┐                           │
│  │   SQLite DB  │    │  FSM Engine  │  (Deterministic)          │
│  │  (/data/)    │    │              │                           │
│  └──────────────┘    └──────────────┘                           │
└─────────▲────────────────────────────────────────────────────────┘
          │
          │ HTTP (REST) + WebSocket (subscriptions)
          │
┌─────────┴────────────────────────────────────────────────────────┐
│                         CLI (Host)                                │
│  pnpm cli up/down/status/login/chats/send/...                    │
└──────────────────────────────────────────────────────────────────┘
```

## Packages

```
packages/
├── shared/              # Types (generated from Rust via ts-rs)
├── agent-server-rust/   # Runs INSIDE container — Rust/Axum REST server + FSM engine
└── cli/                 # Runs on HOST — HTTP/WebSocket client
```

## FSM Architecture (Login Flow)

The login flow uses a **deterministic FSM** instead of an LLM. This is faster, cheaper, and more reliable.

### Core Concepts

| Concept | Location | Purpose |
|---------|----------|---------|
| **IAState** | `src/ia/states/*.rs` | View state: identify from a11y, reduce to AppState, available commands |
| **Effects** | `src/effects/mod.rs` | Reactive side effects that fire on state change |
| **Commands** | `src/ia/states/*.rs` | Per-state UI operations (click, type, scroll, wait) |
| **Base Commands** | `src/ia/states/base.rs` | Shared commands (maximize, minimize, close) |
| **Plan** | `src/plans/*.rs` | Goal + action selection logic |
| **Execution** | `src/execution/mod.rs` | Main loop that runs the FSM |
| **Context** | `src/context/mod.rs` | Persists AppState to SQLite |

All paths above are relative to `packages/agent-server-rust/`.

### Execution Loop

```
┌──────────────────────────────────────────────────────────────┐
│                    Execution Loop                             │
│                                                               │
│  1. OBSERVE    → a11y tree + screenshot (with parent refs)    │
│  2. IDENTIFY   → find IAState, get metadata (e.g., frame)     │
│  3. REDUCE     → iaState.reduce(prev, obs, metadata) → state  │
│  4. EFFECTS    → watchers(prev, next) → Effect[] (on change)  │
│  5. PERSIST    → save AppState to SQLite                      │
│  6. SELECT     → plan.select_action(state) → action key       │
│  7. EXECUTE    → run command (scoped to frame if available)   │
│  8. GOAL?      → plan.is_goal_reached(state) → done?          │
│  9. LOOP       → back to step 1                               │
└──────────────────────────────────────────────────────────────┘
```

Note: Goal is checked AFTER action executes, so plans can run a final action before completing.

### Key Files

**Types** (`src/ia/types.rs`):
```rust
// App state (persisted)
struct AppState {
    main_window: MainWindowState,  // view, qr_data, selected_chat_id, etc.
    popup: Option<PopupState>,
}

// Actions are UI operations (enum variants)
enum Action {
    Click { selector: String },
    ClickXY { x: i32, y: i32 },
    Type { text: String },
    Key { combo: String },
    Scroll { direction: ScrollDirection, x: Option<i32>, y: Option<i32> },
    Wait { ms: u64 },
    Emit { event: SubscriptionEvent },
    Sequence { actions: Vec<Action> },
}
```

**States** (`src/ia/states/`):
- `base.rs` - Shared commands: `window_control_commands` (maximize, minimize, close, sticky)
- `login.rs` - Login states: `login_qr`, `login_account`, `login_phone_confirm`, `login_loading`, `network_proxy_settings`
- `chat.rs` - Main chat view with chat list and messages
- `popup.rs` - Error/confirm/info popups

**Effects** (`src/effects/mod.rs`):

Currently empty — all login emissions (QR, phone_confirm, login_success, status) are handled directly by the login plan via `plan_state` to ensure proper sequencing.

**Plans** (`src/plans/login.rs`):

Login plan phases: `initializing → authenticating → maximizing → detecting_user → extracting_keys → done`

```
initializing     Exit proxy settings page if landed on it
     ↓
authenticating   Wait for QR scan, phone confirm, loading
     ↓           (transitions when view reaches "chat")
maximizing       Send maximize command
     ↓
detecting_user   Find WeChat PID, resolve account directory
     ↓           If stored credentials are valid → skip to done
extracting_keys  Extract and store DB credentials (~20s)
     ↓
done             Emit login_success, goal check passes
```

The plan handles all emissions directly via `plan_state` (QR changes, phone_confirm, status messages, login_success) rather than using effect watchers.

**Network proxy**: Configured at the container level via the `PROXY` env var (transparent proxy using redsocks + iptables). The login plan does NOT drive WeChat's in-app proxy UI — if the proxy settings page is encountered, the plan simply navigates back.

**Plans** (`src/plans/chat_open.rs`):

Chat open plan: `pending → done` (single FSM step)

```
pending    Observe IA state, find click target from a11y tree
   ↓       Calls open_chat() with coordinates + force flag
done       Result stored in plan_state.result, goal check passes
```

Key behaviors:
- **`chat_open` IA** (a chat is already selected): Uses Frida current-selection detection to skip if target is already open
- **`chat` IA** (no chat selected): Passes `--force` to bypass current-selection check (memory detection unreliable after deselect)
- Click coordinates from a11y tree passed via `--click-xy` to `chat-select.py`

**Async select_action:** `Plan::select_action` is async, allowing plans to `.await` tool calls (e.g., `open_chat()`) without blocking the event loop. The execution loop `await`s each `select_action` call.

**Plans** (`src/plans/send_message.rs`):

Send message plan: `opening → focusing → inputting → confirming → done`

```
opening      Open target chat via open_chat()
   ↓
focusing     Find EDITABLE text sibling of Send(S) button, click to focus
   ↓
inputting    Text: Ctrl+A + paste + Enter; Image: paste-image + Enter
   ↓
confirming   Verify Send(S) DISABLED (message sent), retry up to 5x
   ↓
done         Goal reached
```

Key behaviors:
- Reuses `open_chat()` from `chat_select.rs` in opening phase (same as chat-open plan)
- Finds edit component by structure: sibling of `push-button[name="Send(S)"]` with EDITABLE state
- A11y tree outputs DISABLED state for interactive elements (push-button, text, etc.) when ENABLED is absent
- Ctrl+A before paste ensures any existing text is replaced
- Image sending: CLI reads file → base64 via REST → server writes temp file → `paste-image` tool copies to clipboard via `xclip -t <mime>` → Ctrl+V paste → Enter to confirm

### CSS-like Selectors

The a11y tree uses CSS-like selectors (`src/ia/selectors.rs`):

```rust
// Query descendants
query_selector(&a11y, "push-button[name=\"Log In\"]")
query_selector(&a11y, "list[name=\"Chats\"] > list-item:nth-child(1)")
query_selector(&a11y, "push-button[name=/OK|Confirm|确定/i]")  // regex

// Traverse up (a11y nodes have parent refs)
find_ancestor(&button, "frame")  // Find containing frame
```

## Tool Scripts (in container at /opt/tools/)

**UI Observation:**
- `screenshot` - returns base64 PNG
- `a11y-dump` - returns nested JSON a11y tree

**UI Interaction:**
- `click <x> <y>` - click coordinates
- `input "<text>"` - type via clipboard paste (Unicode-safe)
- `paste-image <file> [mime]` - paste image via clipboard (xclip -t)
- `key <combo>` - press keys (Return, Escape, ctrl+a, etc.)
- `scroll <up|down> [amount]`

## CLI Commands

```bash
pnpm cli up              # Start container
pnpm cli up --proxy user:pass@host:port  # Start with transparent proxy
pnpm cli down            # Stop container
pnpm cli status          # Check server + login state
pnpm cli auth login      # Login flow
pnpm cli chats list      # List chats
pnpm cli chats open <id> # Open a chat in the UI
pnpm cli find <name>     # Find chat by name
pnpm cli messages list <id>                    # List messages
pnpm cli messages send <id> --text "msg"       # Send text message
pnpm cli messages send <id> --image f.png      # Send image
pnpm cli messages media <id> <localId>         # Download media attachment
```

## Building

```bash
pnpm build                    # Build CLI + shared types
pnpm build:image:arm64        # Build Docker image (ARM)
pnpm build:image:amd64        # Build Docker image (Intel)
```

### Development Workflow

```bash
pnpm dev:deploy               # Cross-compile Rust server + copy to running container
cargo check                   # Type check Rust code (from packages/agent-server-rust/)
pnpm generate-types           # Regenerate TS types from Rust (after changing ts-rs structs)
pnpm build                    # Rebuild CLI after changes
```

The `dev:deploy` script builds the Rust binary for `aarch64-unknown-linux-gnu` (or `x86_64`) in a Docker builder, copies it into the running container, and restarts the server process.

### Changesets

Always add a changeset when making user-facing changes (features, fixes, behavior changes). Run `pnpm changeset` or create a `.changeset/<name>.md` file manually:

```markdown
---
"@agent-wechat/wechat": patch
---

Short description of the change.
```

Use `patch` for fixes, `minor` for new features, `major` for breaking changes.

## Environment Variables

- `AGENT_WECHAT_URL` - Override server URL (default: http://localhost:6174)
- `AGENT_WECHAT_TOKEN` - Override auth token (default: read from `~/.config/agent-wechat/token`)
- `AGENT_DB_PATH` - Override SQLite DB path (default: /data/agent.db)
- `PROXY` - Transparent proxy for container traffic (format: `user:pass@host:port`, prefix `socks5://` for SOCKS5)

## Security

### Token Authentication

All HTTP and WebSocket endpoints require a bearer token. The token is auto-generated on first container start.

- **Token file**: `~/.config/agent-wechat/token` (host) → `/data/auth-token` (container, read-only mount)
- **HTTP**: `Authorization: Bearer <token>` header
- **WebSocket**: `?token=<token>` query param (native WebSocket doesn't support headers)
- **Required**: Server refuses to start without a token (no token file or env var = startup error)
- **Health**: `/health` is always accessible without auth
- **noVNC**: Browser-based VNC at `http://localhost:6174/vnc/?token=<token>&autoconnect=true` — proxied through agent server with full token auth

| Command | Purpose |
|---------|---------|
| `pnpm cli auth token` | Show current token |
| `pnpm cli auth token --regenerate` | Generate new token (requires container restart) |

Environment variable `AGENT_WECHAT_TOKEN` overrides the token file on both host (CLI) and container (server) side.

## Database

### Overview

- **Technology**: SQLite + rusqlite (with bundled SQLCipher) + refinery migrations
- **Location**: `/data/agent.db` in container (configurable via `AGENT_DB_PATH`)
- **Schema**: `packages/agent-server-rust/migrations/V1__baseline.sql`
- **Queries**: `packages/agent-server-rust/src/db/queries.rs`

### Tables

| Table | Purpose |
|-------|---------|
| `sessions` | Multi-user sessions (display, VNC port, noVNC port, login state, `logged_in_user`) |
| `wechat_keys` | Credentials per (session, account, db_name) |
| `sync_state` | Key-value store for sync progress |
| `context` | FSM AppState persistence (JSON blob) |

### Making Schema Changes

1. Write a new migration file: `migrations/V{N}__{description}.sql`
2. The migration runs automatically on next server start via refinery

### Development Tips

- **Fresh start**: Delete DB file and restart — tables recreate from migrations
- **Schema mismatch errors**: Usually means you need a new migration
- rusqlite uses `Connection` directly (no ORM) — queries in `src/db/queries.rs`

## Key Design Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| Server language | Rust (Axum) | Smaller binary, lower memory, faster startup |
| Chat data | Direct WeChat DB reads | Fast, reliable — no UI scraping needed |
| Login flow | Deterministic FSM | Fast, cheap, reliable — no LLM needed |
| State management | Redux-like (reduce → effects) | Pure reducers, reactive effects on state diff |
| Commands | Per-state (not global) | Each state defines available commands |
| Execution order | Action → Goal check | Allows final actions before completion |
| A11y tree | Parent refs added | Enables `find_ancestor` for frame scoping |
| A11y selectors | CSS-like syntax | Familiar, composable |
| Context | Persisted to SQLite | Survives restarts |
| select_action | Async | Plans can await tool calls without blocking |
| Types | Generated from Rust via ts-rs | Single source of truth, shared with CLI |

## Adding New Features

**To add a new state:**
1. Add state to `src/ia/states/*.rs` with `identify`, `reduce`, `commands`
2. Use `find_ancestor` in identify to get frame metadata
3. Include `window_control_commands()` for common window controls
4. Register in the states array in `src/ia/states/mod.rs`
5. Update plans if action selection needed

**To add a new plan:**
1. Create `src/plans/myplan.rs` implementing the `Plan` trait
2. Register in `src/plans/mod.rs`
3. Call via `run_execution_loop(&plan, &params, &mut context, &emit, cancel)`
4. Remember: action executes BEFORE goal check (can have final action)

## WeChat Data Access

Chat and message data is read directly from WeChat's local databases using stored access credentials.

### Key Files

| File | Purpose |
|------|---------|
| `src/tools/wechat_db.rs` | `query_wechat_db()`, `find_account_dir()`, `find_wechat_pid()` |
| `src/tools/wechat_keys.rs` | `extract_keys()`, `store_keys()`, `get_stored_keys()`, `verify_key()` |
| `src/tools/wechat_chats.rs` | `list_chats()`, `get_chat_by_username()`, `find_chats_by_name()` |
| `src/tools/wechat_messages.rs` | `list_messages()`, `find_message_db()`, `get_msg_table_name()` |
| `src/tools/wechat_media.rs` | `get_message_media()` — images, emoji, voice |

All paths relative to `packages/agent-server-rust/`.

### Databases Read

- `session.db` → `SessionTable` — active chats, sort order, unread counts, last message preview
- `contact.db` → `contact` — display names, remarks, aliases, avatars
- `message_N.db` → `Msg_{MD5(chatId)}` — per-chat message tables (may span multiple DBs)
- `message_resource.db` → `MessageResourceInfo`, `ChatName2Id` — image file hash lookup (primary)
- `media_N.db` → `VoiceInfo` — voice message data
- `hardlink.db` → `image_hardlink_info_v4`, `dir2id` — image file path resolution (fallback)
- `emoticon.db` → `kNonStoreEmoticonTable` — emoji CDN URLs

### Message Table Sharding

WeChat uses per-chat message tables named `Msg_{MD5(chatId)}`. Messages may be spread across `message_0.db`, `message_1.db`, etc. The code scans all message DBs to find which one contains a given chat's table.

### Image File Lookup

Image `.dat` files are stored at `msg/attach/<md5(chatId)>/<YYYY-MM>/Img/<fileHash>.dat`. The `fileHash` in the filename is NOT the same as the `md5` attribute in the message XML — it's a separate hash assigned by WeChat.

**Lookup order** (in `get_message_media`):

1. **Thumbnail cache** — `cache/<YYYY-MM>/Message/<md5(chatId)>/Thumb/{localId}_{createTime}_thumb.jpg`
2. **`message_resource.db`** (primary) — `MessageResourceInfo.packed_info` contains the `fileHash` as a protobuf-encoded blob. Query by `ChatName2Id.rowid` + `message_local_id`. Available immediately when image is received.
3. **`hardlink.db`** (fallback) — `image_hardlink_info_v4` maps XML `md5` → `file_name` + directory IDs. Has an indexing delay — may not be populated until the user views the image in the WeChat UI.

### Image Processing

Image `.dat` files use a two-layer encoding: AES-128-ECB for the header and single-byte XOR for the tail. The XOR byte is derived from known file format trailers (JPEG `FFD9`, PNG `IEND`). Some full-size images are in WeChat's proprietary `wxgf` format — thumbnails (`_t.dat`) are always JPEG.

**File variants** in the Img directory:
- `<hash>.dat` — mid-resolution image
- `<hash>_t.dat` — thumbnail
- `<hash>_h.dat` — high-resolution (full-size) image

### Gotchas

- `pgrep -f /usr/bin/wechat` returns multiple PIDs (wrapper + real process) — pick the one with most open fds
- Stored `wechatPid` goes stale after container rebuild — always fall back to `find_wechat_pid()`
- Python extract-keys script exits non-zero if any DB key not found — catch error, read JSON output file anyway (partial success)
- hardlink.db has indexing delay — use `message_resource.db` as primary lookup for image files
- hardlink.db `dir2id` stores md5(chatId) not raw chatId
- WeChat DBs are read with `immutable=1` (skips WAL); a background task checkpoints every 3s (PASSIVE mode) to flush WAL → main DB so reads see fresh data

## Current Status

- [x] Deterministic FSM for login flow
- [x] Login plan with post-login setup (detect user → setup → done)
- [x] Direct WeChat DB reads (session.db, contact.db, message_N.db)
- [x] Smart credential management (verify existing, only re-extract when needed)
- [x] Context persistence to SQLite
- [x] Parent refs in a11y tree + find_ancestor helper
- [x] Frame-scoped click/type actions
- [x] Per-state commands with shared base (window controls)
- [x] Plan-local state for execution-scoped data
- [x] Chat open via FSM plan (with current-selection detection)
- [x] Async select_action for non-blocking tool calls in plans
- [x] Send message via FSM plan (text + image + file)
- [x] Message history from WeChat DBs (per-chat sharded tables)
- [x] Media retrieval (image, emoji, voice)
- [x] Rust agent-server (replaced Node.js)
- [x] TypeScript types generated from Rust via ts-rs

## HTTP API Protocol

The agent-server (Rust, port 6174) exposes a REST + WebSocket API consumed by the CLI.

### Status

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/status` | Container health check |
| GET | `/api/status/auth` | Auth status via FSM observation (a11y → identify → reduce) |
| POST | `/api/status/login` | One-shot login check (screenshot → QR decode) |

**GET /api/status** → `{ container, loginState: { status }, version }`

**GET /api/status/auth** → `{ status: "logged_in" | "logged_out" | "app_not_running" | "unknown", loggedInUser?: string }`

**POST /api/status/login** → `{ success: bool, state: { status }, qrDataUrl? }`

### Login WebSocket

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/ws/login?timeoutMs=300000&newAccount=false` | Login flow via WebSocket |

Runs full FSM execution loop. Server sends `LoginSubscriptionEvent` JSON messages:

```
{ "status":        { message } }
{ "qr":            { qrData, qrBinaryData?, qrDataUrl? } }
{ "phone_confirm": { message? } }
{ "login_success": { userId? } }
{ "login_timeout": {} }
{ "error":         { message } }
```

### Chats

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/chats?limit=50&offset=0` | List chats (from WeChat DB) |
| GET | `/api/chats/{id}` | Get single chat by username |
| GET | `/api/chats/find?name=...` | Search chats by display name |
| POST | `/api/chats/{id}/open` | Open chat in UI via FSM plan |

**GET /api/chats** → `Chat[]`

```typescript
interface Chat {
  id: string;           // = username
  username: string;
  name: string;
  remark?: string;
  lastMessagePreview?: string;
  lastMessageSender?: string;
  lastActivityAt?: string;  // ISO-8601
  unreadCount?: number;
  sortKey?: number;
}
```

**POST /api/chats/{id}/open** → `{ ok, username?, index?, skipped?, error? }`

### Messages

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/messages/{chatId}?limit=50&offset=0` | List messages (from WeChat DB) |
| GET | `/api/messages/{chatId}/media/{localId}` | Get media for a message |
| POST | `/api/messages/send` | Send text, image, or file via FSM plan |

**GET /api/messages/{chatId}** → `Message[]`

```typescript
interface Message {
  localId: number;
  serverId: number;
  chatId: string;
  sender?: string;        // wxid from Name2Id join
  type: number;           // WeChat message type
  content: string;        // cleaned: text as-is, emoji→cdnurl/[emoji], appmsg→title, image→empty
  timestamp: string;      // ISO-8601
  isMentioned?: boolean;  // true if current user is @-mentioned (group chats only)
  reply?: {               // present for quote/reply messages (type 49, subtype 57)
    sender?: string;      // display name of quoted sender
    content: string;      // quoted message text
  };
}
```

**GET /api/messages/{chatId}/media/{localId}** → `MediaResult`

```typescript
interface MediaResult {
  type: string;        // "image" | "voice" | "unsupported"
  data?: string;       // base64
  url?: string;        // CDN URL
  format: string;      // "jpeg", "png", "mp3", etc.
  filename: string;
}
```

**POST /api/messages/send** (JSON body) → `SendResult`

```typescript
// Request
{ chatId: string, text?: string, image?: { data: string, mimeType: string }, file?: { data: string, filename: string } }
// data fields are base64-encoded

// Response
{ success: boolean, error?: string }
```

### Sessions

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/sessions` | List all sessions |
| POST | `/api/sessions` | Create session `{ name }` |
| GET | `/api/sessions/{id}` | Get session |
| DELETE | `/api/sessions/{id}` | Delete session |
| POST | `/api/sessions/{id}/start` | Start session |
| POST | `/api/sessions/{id}/stop` | Stop session |

```typescript
interface Session {
  id: string;
  name: string;
  linuxUser: string;
  display: string;
  dbusAddress?: string;
  vncPort: number;
  novncPort: number;
  status: string;
  loginState: string;
  loggedInUser?: string;
  wechatPid?: number;
}
```

### Debug

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/debug/screenshot` | `{ base64: string }` (PNG) |
| GET | `/api/debug/a11y?format=json\|aria` | `{ tree?, aria?, error? }` |

### Events WebSocket

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/ws/events` | Real-time event stream (not yet wired up) |
