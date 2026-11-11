# `wx` CLI Complete Command and Option Reference Manual

This document provides a comprehensive reference for all command-line options, subcommands, defaults, and constraints of the `agent-wechat` native CLI tool `wx`.

---

## 1. Global Options

Global options may be placed either before or after subcommands:

| Option | Shorthand | Type | Default | Description |
| :--- | :---: | :---: | :---: | :--- |
| `--json` | None | Flag | `false` | Enable structured JSON output (recommended for automation scripts and agents) |
| `--account <ID/ALIAS>` | None | String | Default account | Target a specific WeChat account ID or alias |
| `--config <PATH>` | None | Path | Auto-detected | Client configuration file path (overrides `~/.config/agent-wechat/client.toml`) |
| `--server <URL>` | `--url` | String | Local daemon | Target HTTP API address (e.g. `http://127.0.0.1:6174`) |
| `--server-name <NAME>` | None | String | None | Use a named server entry from client configuration |
| `--server-config <PATH>`| None | Path | Auto-detected | Server configuration file path (for `serve` or `daemon start`) |
| `--data-dir <PATH>` | None | Path | Auto-detected | Override persistent data root (default `~/.local/share/agent-wechat`) |
| `--token-file <PATH>` | None | Path | Auto-detected | Override API token file path |
| `--token-stdin` | None | Flag | `false` | Read API token from stdin (prevents token leakage in process lists) |
| `--listen <ADDR>` | None | SocketAddr | `127.0.0.1:6174` | Override server local bind address and port |
| `--network-listen <ADDR>`| None | SocketAddr| None | Additional HTTPS bind address (for remote connections; requires TLS files) |
| `--tls-cert-file <PATH>`| None | Path | None | Path to TLS certificate public key file |
| `--tls-key-file <PATH>` | None | Path | None | Path to TLS private key file |
| `--ca-cert-file <PATH>` | None | Path | None | Additional root CA certificate trusted when connecting to remote servers |

---

## 2. Command Tree and Subcommand Options

### 1. General Information
* `wx version [--json]`: Print CLI version, native Rust engine version, and target architecture.
* `wx status [--json]`: Display client connection status, daemon connectivity, and account readiness.
* `wx licenses`: Display license information for the project and its open-source dependencies.

### 2. `wx daemon` - Background Daemon Management
* `wx daemon start`: Start the daemon in the background and write a lock-protected PID file.
* `wx daemon status`: Check if the background daemon is running, including PID, uptime, and port.
* `wx daemon logs [--follow]`: Read daemon logs; pass `--follow` to stream output continuously.
* `wx daemon stop`: Gracefully terminate the background daemon and release file locks and sockets.

### 3. `wx serve` - Foreground Server
* `wx serve`: Start the server in the foreground with logging to stdout.
* Use cases: Local debugging, main process in Docker containers, or managed by systemd.

### 4. `wx config` - Configuration Management
* `wx config show`: Resolve and display active configuration paths, data directories, and servers.
* `wx config client`: View or initialize the client configuration file.
* `wx config server add <NAME> --url <URL> --token-file <PATH>`: Register a remote or local daemon endpoint.
* `wx config server list`: List all configured servers.
* `wx config server use <NAME>`: Switch the active default server.

### 5. `wx accounts` / `wx account` - Accounts and Capabilities
* `wx accounts list`: List all WeChat account slots managed by the daemon and their login status.
* `wx accounts create [ID] [--alias <ALIAS>]`: Create a new blank account slot.
* `wx accounts get [ID]`: Retrieve detailed information for an account.
* `wx accounts default <ID>`: Set the global default account.
* `wx accounts alias <ID> <ALIAS>`: Assign a friendly alias to an account (e.g. `work`, `personal`).
* `wx accounts remove <ID> --confirm <ID>`: Remove a signed-out account from the registry (retains local database).
* `wx account status`: View the online/session state of the active account.
* `wx account settings [--auto-download-max-mb <N>]`: View or configure auto-download thresholds for large attachments (default 50 MB).
* `wx account capabilities`: Display native protocol capabilities supported by the server and client.
* `wx account connect [--detach] [--wait <SECS>]`: Initiate network connection and event streaming.
* `wx account disconnect`: Close network connection without revoking credentials.

### 6. `wx auth` - Authentication and Sign-in
* `wx auth login`:
  * `--official[=PATH]`: In-place takeover of an official client profile (recommended on Linux desktop; bypasses QR scan).
  * `--strategy <auto|saved-device|qr>`: Login strategy (default `auto` tries saved device credentials first).
  * `--force-qr` (or `--qr`): Force a fresh QR code challenge.
  * `--no-qr`: Only sign in with saved device credentials; fail immediately if a QR code is required.
  * `--qr-output <PATH>`: Save the login QR code PNG image to a specified path.
  * `--qr-refresh <auto|manual>`: Whether to refresh the QR code automatically on expiry (default `auto`).
  * `--max-refreshes <N>`: Maximum automatic QR code refreshes (0–3, default 3).
  * `--timeout <SECS>`: Total timeout in seconds waiting for login confirmation (default 300s).
  * `--detach`: Return immediately with an operation ID after initiating the login challenge.
* `wx auth operation <OPERATION_ID>`: Poll the status of an ongoing login challenge.
* `wx auth qr <OPERATION_ID> [--output <PATH>]`: Retrieve the current QR code image for an ongoing operation.
* `wx auth cancel <OPERATION_ID>`: Cancel an in-progress login attempt.
* `wx auth logout [--detach] [--wait <SECS>]`: Safely log out from WeChat servers and revoke local credentials.
* `wx auth forget --yes`: Clear local authentication credentials while retaining chat history.

### 7. `wx messages` - Messaging and Media
* `wx messages send <THREAD>`: Send a message to a direct contact or group chat.
  * `--text <TEXT>`: Send plain text content.
  * `--image <PATH>`: Send a local image.
  * `--file <PATH>`: Send a file or document.
  * `--video <PATH> [--video-quality <normal|original>]`: Send a video.
  * `--voice <PATH>`: Send a SILK voice note.
  * `--share <URL> --title <TITLE> [--description <DESC>] [--thumbnail-url <URL>]`: Send a link card.
  * `--quote-message-id <MSG_ID> --quote-server-id <SERVER_ID> --text <TEXT>`: Quote an existing message in the reply.
  * `--mention <ACCOUNT_ID>... --text <TEXT>`: Mention specific member(s) in a group chat.
  * `--mention-all --text <TEXT>`: Mention all members (`@all`) in a group chat.
  * `--idempotency-key <KEY>`: Client-provided idempotency key to prevent accidental duplicate sends.
  * `--detach`: Return immediately after enqueuing the send task without waiting for remote server confirmation.
* `wx messages list <THREAD> [--limit <N>] [--cursor <CURSOR>]`: Page through conversation message history.
* `wx messages get <THREAD> <MSG_ID>`: Retrieve details of a specific message.
* `wx messages recall <MSG_ID> --server-id <EXACT_SERVER_ID>`: Recall a message sent within the 2-minute window.
* `wx messages media <THREAD> <MSG_ID> [--quality <best|thumbnail|standard|original>] [--output <PATH>] [--cached-only]`: Download image, video, voice, or attachment files from a message.
* `wx messages read <THREAD> --expected-version <VERSION>`: Mark a conversation as read up to the given version.
* `wx messages send-status <SEND_ID>`: Query the status of an asynchronous send job.
* `wx messages sync`: Explicitly trigger an incremental synchronization pass.

### 8. `wx chats` / `wx threads` - Chats and Conversations
* `wx chats list [--limit <N>] [--cursor <CURSOR>] [--kind <direct|group>]`: List conversation threads.
* `wx chats get <ID>`: Get conversation metadata (unread count, last message, pinned flag, etc.).
* `wx chats find <QUERY>`: Search conversations locally.
* `wx chats pin <ID>` / `wx chats unpin <ID>`: Pin or unpin a conversation.
* `wx chats mute <ID>` / `wx chats unmute <ID>`: Enable or disable mute preferences for a chat.
* `wx chats read <ID> --expected-version <VERSION>`: Mark chat as read.
* `wx threads [--limit <N>] [--cursor <CURSOR>]`: View thread summary list.

### 9. `wx groups` - Group Chats and Members
* `wx groups list [--limit <N>] [--cursor <CURSOR>]`: List joined group chats.
* `wx groups get <ROOM_ID>`: Retrieve group chat details.
* `wx groups create <MEMBER_ID_1> <MEMBER_ID_2>... [--name <NAME>]`: Create a new group chat (initial 2–40 members).
* `wx groups rename <ROOM_ID> <NEW_NAME>`: Rename a group chat.
* `wx groups my-name <ROOM_ID> <DISPLAY_NAME>`: Update own nickname within a group chat.
* `wx groups clear-announcement <ROOM_ID>`: Clear group announcement.
* `wx groups leave <ROOM_ID>`: Leave a group chat.
* `wx groups lookup-group <ROOM_ID>`: Query latest group profile from WeChat servers.
* **Member management subcommands**:
  * `wx groups members list <ROOM_ID> [--limit <N>]`: List group members.
  * `wx groups members get <ROOM_ID> <USERNAME>`: Get details for a specific member in the group.
  * `wx groups members add <ROOM_ID> <USER_1> <USER_2>...`: Invite members into the group.
  * `wx groups members remove <ROOM_ID> <USER_1>...`: Remove members from the group.
  * `wx groups members refresh <ROOM_ID>`: Force refresh member roster from cloud.

### 10. `wx contacts` - Contacts
* `wx contacts list [--limit <N>] [--cursor <CURSOR>]`: List contacts with pagination.
* `wx contacts search <QUERY>`: Search local contacts.
* `wx contacts lookup-contacts <USER_ID>...`: Query latest user profile from WeChat servers.
* `wx contacts avatar <USER_ID> [--output <PATH>] [--refresh]`: Fetch and export contact avatar.
* `wx contacts sync`: Trigger incremental contact sync.

### 11. `wx events` - Event Bus and Streaming
* `wx events watch [--after <CURSOR>] [--cursor-file <PATH>]`: Stream real-time messages and state change events.
* `wx events list [--after <CURSOR>] [--limit <N>]`: Fetch batches of historical events after a cursor.
* `wx events ack --consumer <NAME> --through <CURSOR>`: Acknowledge processed event cursors to the daemon.

### 12. `wx tokens` - Access Token Scopes
* `wx tokens list`: List configured API tokens.
* `wx tokens create <NAME> --accounts <ACCOUNTS...> --scopes <SCOPES...> [--expires-at <TIMESTAMP>]`: Create a restricted token.
  * Scopes: `read`, `media`, `send`, `read-state`, `login`, `admin`.
* `wx tokens revoke <TOKEN_ID>`: Revoke an existing API token.

### 13. `wx store` - Offline Storage Operations
*Requirement: The daemon and official WeChat client must be stopped before running store commands.*
* `wx store list`: List registered local store directories.
* `wx store attach --path <PATH> --source-stopped`: Attach an offline official profile.
* `wx store import --source <SRC> --source-stopped`: Import profile copy.
* `wx store export --destination <DEST> --source-stopped [--allow-incomplete]`: Export profile copy.
* `wx store schema`: Inspect database schema fingerprint.
* `wx jobs repair-writeback [--limit <N>]`: Reconcile and repair cached media files to filesystem.
