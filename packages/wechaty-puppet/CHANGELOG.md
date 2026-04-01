# @agent-wechat/wechaty-puppet

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

## 0.11.4

## 0.11.3

## 0.11.2

## 0.11.1

## 0.11.0

## 0.10.2

## 0.10.1

## 0.10.0

## 0.9.5

## 0.9.4

## 0.9.3

## 0.9.2

## 0.9.1

## 0.9.0

### Minor Changes

- [#75](https://github.com/thisnick/agent-wechat/pull/75) [`30a2981`](https://github.com/thisnick/agent-wechat/commit/30a2981f9c728e09b686a37f5aca1687baa5a70d) Thanks [@thisnick](https://github.com/thisnick)! - Add Wechaty gateway package and puppet improvements for gRPC service hosting.

  - New `packages/wechaty-gateway/` wraps PuppetAgentWeChat as a standard Wechaty gRPC puppet service
  - Snapshot message baseline on connect to prevent historical message replay
  - Guard against double login crash on PuppetServer client reconnect
  - Emit heartbeat every poll cycle to keep gRPC watchdog alive
  - Clear unreads via openChat after processing messages

## 0.8.5

## 0.8.4

### Patch Changes

- [#70](https://github.com/thisnick/agent-wechat/pull/70) [`6c27002`](https://github.com/thisnick/agent-wechat/commit/6c27002c3356cd34e84da77b0a44aaf6556146ed) Thanks [@thisnick](https://github.com/thisnick)! - Harden Wechaty puppet login websocket handling and clarify CLI status behavior.

  - Treat websocket `error`/`close` callbacks as non-fatal once a terminal login event has been seen.
  - Normalize empty websocket error messages to a stable fallback.
  - Close the login subscription handle immediately after `login_success` to reduce late transport noise.
  - Make `wx status` report explicit container up/down state, and only show server/login details when the container is running.

## 0.8.3

### Patch Changes

- [#68](https://github.com/thisnick/agent-wechat/pull/68) [`c8a4ec9`](https://github.com/thisnick/agent-wechat/commit/c8a4ec92ced516451155c4c0655ca46aee46e09e) Thanks [@thisnick](https://github.com/thisnick)! - Clarify that WeChat login can be completed through the puppet QR flow without requiring a separate CLI login step.

  Also improve login websocket behavior for puppet clients:

  - Remove noisy unknown-UI waiting status messages during login (`Unknown UI state ({}s), waiting...`) while keeping the existing hard-coded unknown-state timeout behavior.
  - Ensure the server sends a terminal login event (`login_success`, `login_timeout`, or `error`) before closing the login websocket, instead of closing without a final event.

## 0.8.2

## 0.8.1

### Patch Changes

- [#63](https://github.com/thisnick/agent-wechat/pull/63) [`67f92fe`](https://github.com/thisnick/agent-wechat/commit/67f92fee2a4e1f1440b7f6982f6962e6652e3dd5) Thanks [@thisnick](https://github.com/thisnick)! - Fix login event handling — QR scan events were not emitted due to incorrect event discriminator

## 0.8.0

### Minor Changes

- [#59](https://github.com/thisnick/agent-wechat/pull/59) [`eb95ac6`](https://github.com/thisnick/agent-wechat/commit/eb95ac6f6ac0bc072450a12f636ee19544201ae2) Thanks [@thisnick](https://github.com/thisnick)! - Add Wechaty puppet package and contacts API

  - New `@agent-wechat/wechaty-puppet` package: bridges Wechaty bots to WeChat via the agent-wechat server
  - New `GET /api/contacts` endpoint: queries contact.db for full address book
  - New CLI commands: `contacts list` and `contacts find`
