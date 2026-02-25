# @agent-wechat/cli

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
