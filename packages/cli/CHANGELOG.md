# @agent-wechat/cli

## 0.11.15

## 0.11.14

## 0.11.13

## 0.11.12

## 0.11.11

## 0.11.10

## 0.11.9

## 0.11.8

## 0.11.7

## 0.11.6

## 0.11.5

### Patch Changes

- [#118](https://github.com/thisnick/agent-wechat/pull/118) [`fe27f34`](https://github.com/thisnick/agent-wechat/commit/fe27f344a8dc7746ebcf2ea5bf2bd5e886e2dd4e) Thanks [@thisnick](https://github.com/thisnick)! - Replace x86_64 NativeFunction lookup with pure memory hashmap walk in chat-select to prevent crashes

## 0.11.4

### Patch Changes

- [#116](https://github.com/thisnick/agent-wechat/pull/116) [`42c80f6`](https://github.com/thisnick/agent-wechat/commit/42c80f6d050d0210402f0a9fa00bf107cadd6db1) Thanks [@thisnick](https://github.com/thisnick)! - Move WeChat restart logic from entrypoint bash loop into agent-server health monitor

## 0.11.3

### Patch Changes

- [#113](https://github.com/thisnick/agent-wechat/pull/113) [`55544aa`](https://github.com/thisnick/agent-wechat/commit/55544aa6320b9bed170fd6a614bde7f32ffe3c99) Thanks [@thisnick](https://github.com/thisnick)! - Fix verify_key to use immutable=1 to avoid acquiring locks on WeChat databases

- [`8f7c6c2`](https://github.com/thisnick/agent-wechat/commit/8f7c6c2abbc14895b584a04ea7f4edc9cf7a39eb) Thanks [@thisnick](https://github.com/thisnick)! - Log WeChat crash/recovery in health monitor instead of silently ignoring process disappearance

- [#111](https://github.com/thisnick/agent-wechat/pull/111) [`587994b`](https://github.com/thisnick/agent-wechat/commit/587994b98dc7bb7c272e133ad621989ca9513602) Thanks [@thisnick](https://github.com/thisnick)! - Add lazy key extraction to chat and contact query handlers so keys are extracted on demand when missing

## 0.11.2

### Patch Changes

- [#108](https://github.com/thisnick/agent-wechat/pull/108) [`97bc89d`](https://github.com/thisnick/agent-wechat/commit/97bc89db4ac279926be285651ad8452f0e95b1e8) Thanks [@thisnick](https://github.com/thisnick)! - Secure noVNC with full-token auth on the WebSocket proxy (no 8-char VNC limit). Opening /vnc/ shows a login prompt for your token. Direct access via ?token=xxx&autoconnect=true also works. VNC and websockify listen on localhost only.

## 0.11.1

### Patch Changes

- [#106](https://github.com/thisnick/agent-wechat/pull/106) [`66ee117`](https://github.com/thisnick/agent-wechat/commit/66ee117ad9f52c9adefd2cac21ed007828737c79) Thanks [@thisnick](https://github.com/thisnick)! - Fix SQLite migration error: use CREATE UNIQUE INDEX instead of UNIQUE column constraint in ALTER TABLE for novnc_port, since SQLite does not support adding UNIQUE columns via ALTER TABLE.

## 0.11.0

### Minor Changes

- [#101](https://github.com/thisnick/agent-wechat/pull/101) [`470a907`](https://github.com/thisnick/agent-wechat/commit/470a9076a32ea1a83a8901ff636be9531754415f) Thanks [@thisnick](https://github.com/thisnick)! - Encrypt agent.db at rest using SQLCipher with the auth token as the encryption key. Existing unencrypted databases are automatically migrated on startup. If decryption fails (e.g. token changed), the database is discarded and recreated fresh.

- [#104](https://github.com/thisnick/agent-wechat/pull/104) [`50aacf0`](https://github.com/thisnick/agent-wechat/commit/50aacf0c2d09983e75a8847fb394252d4a8548bc) Thanks [@thisnick](https://github.com/thisnick)! - Replace raw VNC port (5900) with noVNC browser-based viewer on port 6080. x11vnc now listens on 127.0.0.1 only (internal to the container), and websockify serves the noVNC web client. Access the desktop at `http://localhost:6080/vnc.html?autoconnect=true`. No VNC client installation needed.

## 0.10.2

## 0.10.1

### Patch Changes

- [#94](https://github.com/thisnick/agent-wechat/pull/94) [`83f3991`](https://github.com/thisnick/agent-wechat/commit/83f399199b4c01a4575b12cfc7900737b2694f0a) Thanks [@thisnick](https://github.com/thisnick)! - feat: add video media support (type 43)

## 0.10.0

### Minor Changes

- [#90](https://github.com/thisnick/agent-wechat/pull/90) [`20d16a1`](https://github.com/thisnick/agent-wechat/commit/20d16a18b0ec24e6d517224c9b644bab4a9bfa6e) Thanks [@thisnick](https://github.com/thisnick)! - Add WeChat health monitor to detect and kill unresponsive processes

## 0.9.5

### Patch Changes

- [#87](https://github.com/thisnick/agent-wechat/pull/87) [`191a7d4`](https://github.com/thisnick/agent-wechat/commit/191a7d43068530e9562f269f61193e6ad04ba2f9) Thanks [@thisnick](https://github.com/thisnick)! - Add mutex to ensure only one plan execution runs at a time

## 0.9.4

## 0.9.3

## 0.9.2

## 0.9.1

## 0.9.0

## 0.8.5

## 0.8.4

### Patch Changes

- [#70](https://github.com/thisnick/agent-wechat/pull/70) [`6c27002`](https://github.com/thisnick/agent-wechat/commit/6c27002c3356cd34e84da77b0a44aaf6556146ed) Thanks [@thisnick](https://github.com/thisnick)! - Harden Wechaty puppet login websocket handling and clarify CLI status behavior.

  - Treat websocket `error`/`close` callbacks as non-fatal once a terminal login event has been seen.
  - Normalize empty websocket error messages to a stable fallback.
  - Close the login subscription handle immediately after `login_success` to reduce late transport noise.
  - Make `wx status` report explicit container up/down state, and only show server/login details when the container is running.

## 0.8.3

## 0.8.2

## 0.8.1

## 0.8.0

### Minor Changes

- [#59](https://github.com/thisnick/agent-wechat/pull/59) [`eb95ac6`](https://github.com/thisnick/agent-wechat/commit/eb95ac6f6ac0bc072450a12f636ee19544201ae2) Thanks [@thisnick](https://github.com/thisnick)! - Add Wechaty puppet package and contacts API

  - New `@agent-wechat/wechaty-puppet` package: bridges Wechaty bots to WeChat via the agent-wechat server
  - New `GET /api/contacts` endpoint: queries contact.db for full address book
  - New CLI commands: `contacts list` and `contacts find`

## 0.7.10

## 0.7.9

## 0.7.8

## 0.7.7

## 0.7.6

## 0.7.5

## 0.7.4

### Patch Changes

- [`dc1e1c8`](https://github.com/thisnick/agent-wechat/commit/dc1e1c8342030c88b665a7b526eac96b75634b42) Thanks [@thisnick](https://github.com/thisnick)! - Fix `wx update`: use native fetch instead of gh CLI, chmod +x after docker cp, reliable arch detection via container uname

## 0.7.3

### Patch Changes

- [`6617da2`](https://github.com/thisnick/agent-wechat/commit/6617da2970b314e8f829587840b7b0764770bd54) Thanks [@thisnick](https://github.com/thisnick)! - Fix container architecture detection in `wx update` command

## 0.7.2

### Patch Changes

- [#39](https://github.com/thisnick/agent-wechat/pull/39) [`65289a7`](https://github.com/thisnick/agent-wechat/commit/65289a7ecd8f0107166fbe28dcd71352d7863d9f) Thanks [@thisnick](https://github.com/thisnick)! - Fix binary publish job in release workflow

  - Remove read-only flag from Docker source mount that prevented container startup (exit code 125)
  - Create GitHub Release before uploading binary assets (release not found error)

## 0.7.1

### Patch Changes

- [#37](https://github.com/thisnick/agent-wechat/pull/37) [`d043f9c`](https://github.com/thisnick/agent-wechat/commit/d043f9c7f1fd0ed8f9aa1081643540c0d9487f22) Thanks [@thisnick](https://github.com/thisnick)! - Fix binary publish job in release workflow

  - Remove read-only flag from Docker source mount that prevented container startup (exit code 125)

## 0.7.0

### Minor Changes

- [#35](https://github.com/thisnick/agent-wechat/pull/35) [`fa0d64a`](https://github.com/thisnick/agent-wechat/commit/fa0d64aa74af8b0e973984977d09dee27a0c96a7) Thanks [@thisnick](https://github.com/thisnick)! - Add binary artifact publishing and CLI update command

  - Release workflow now publishes standalone `agent-server` binaries (amd64/arm64) as GitHub Release assets alongside Docker images
  - New `wx update` command downloads the binary matching the CLI version and hot-swaps it into the running container via `docker cp` + process restart

## 0.6.0

### Minor Changes

- [#33](https://github.com/thisnick/agent-wechat/pull/33) [`0cdae88`](https://github.com/thisnick/agent-wechat/commit/0cdae8861159464ed4a64005ebda70616e3ba2a7) Thanks [@thisnick](https://github.com/thisnick)! - Add container-level transparent proxy via redsocks. Use `wx up --proxy user:pass@host:port` or set the `PROXY` env var to route all container traffic through an upstream HTTP CONNECT or SOCKS5 proxy. Removes app-level proxy UI automation from the login flow.

## 0.5.0

## 0.4.1

## 0.4.0

## 0.3.1

## 0.3.0

### Minor Changes

- [`3dba4d7`](https://github.com/thisnick/agent-wechat/commit/3dba4d7c3381fc73bd5e0732bdaf6f89341b480b) Thanks [@thisnick](https://github.com/thisnick)! - Add WeChat crash recovery and auth status enum

  - Auto-restart WeChat in entrypoint with crash-loop backoff (3s delay, 30s backoff after 5 rapid restarts)
  - Replace `isLoggedIn: boolean` with `status: "logged_in" | "logged_out" | "app_not_running" | "unknown"` in auth endpoint
  - Detect WeChat process not running via `find_wechat_pid()` check before a11y observation
  - Notify agent on auth state transitions (session lost, server unreachable, first-poll not authenticated)
  - Add `app_not_running` diagnostic in openclaw extension status checks

## 0.2.4

### Patch Changes

- [`09aa334`](https://github.com/thisnick/agent-wechat/commit/09aa334d9fef0a67ab092f5f68e10540bd8af9bf) Thanks [@thisnick](https://github.com/thisnick)! - Fix image media retrieval for newly received images by using message_resource.db as the primary file lookup instead of hardlink.db, which has an indexing delay.

## 0.2.3

### Patch Changes

- [`91d6750`](https://github.com/thisnick/agent-wechat/commit/91d67504ffc3965c046ea28e13e2d9d3d5fedaf3) Thanks [@thisnick](https://github.com/thisnick)! - - Use versioned Docker image tags matching CLI version, with fallback to latest
  - Inject version from package.json at build time
  - Fix release workflow Docker tag parsing for scoped packages
  - Increase media poll retries from 5 to 15
  - Add setup docs to both package READMEs

## 0.2.2

### Patch Changes

- [`32e6d04`](https://github.com/thisnick/agent-wechat/commit/32e6d04eb4aca78f6143feb3b0b4c86d08a39f44) Thanks [@thisnick](https://github.com/thisnick)! - Use versioned Docker image tags matching CLI version, fix release workflow version parsing

## 0.2.1

### Patch Changes

- [`ff4e228`](https://github.com/thisnick/agent-wechat/commit/ff4e2288b0f89d3f4ea8e78778a6f31f8d86352d) Thanks [@thisnick](https://github.com/thisnick)! - Auto-pull Docker image in `wx up` when not found locally, add README docs for both packages

## 0.2.0

### Minor Changes

- [`9f1911d`](https://github.com/thisnick/agent-wechat/commit/9f1911dfc80194330dc9e6c352b2c181515ce300) Thanks [@thisnick](https://github.com/thisnick)! - Initial public release

  - CLI (`wx`) for managing agent-wechat containers
  - OpenClaw WeChat channel extension with login, directory, and heartbeat adapters
  - Multi-arch Docker image (amd64/arm64)
