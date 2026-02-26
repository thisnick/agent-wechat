# @agent-wechat/wechat

## 0.7.7

### Patch Changes

- [`f637558`](https://github.com/thisnick/agent-wechat/commit/f6375585be4939f6ed43c610910d585bee35a287) Thanks [@thisnick](https://github.com/thisnick)! - Rewrite README with improved setup flow, prerequisites, and limitations

## 0.7.6

### Patch Changes

- [#44](https://github.com/thisnick/agent-wechat/pull/44) [`feb823c`](https://github.com/thisnick/agent-wechat/commit/feb823c5add5bd5f08e451fa6dbfdff37b2d6e40) Thanks [@thisnick](https://github.com/thisnick)! - Re-extract DB credentials when new message databases appear after login

## 0.7.5

### Patch Changes

- [`109bb0f`](https://github.com/thisnick/agent-wechat/commit/109bb0f088c0291bea4adcdfd7f25347dc52d59b) Thanks [@thisnick](https://github.com/thisnick)! - Document hosted instance support and token configuration in README

## 0.7.4

## 0.7.3

## 0.7.2

## 0.7.1

## 0.7.0

## 0.6.0

## 0.5.0

### Minor Changes

- [`9b1e871`](https://github.com/thisnick/agent-wechat/commit/9b1e871d8666216fa295c44400e1108eeb34a4ef) Thanks [@thisnick](https://github.com/thisnick)! - Buffer non-mentioned group messages and inject as history context when a mention arrives. Only the latest media attachment is preserved to avoid flooding the agent.

## 0.4.1

### Patch Changes

- [#29](https://github.com/thisnick/agent-wechat/pull/29) [`5e53df8`](https://github.com/thisnick/agent-wechat/commit/5e53df8eadd9b37d7812b00bc9c96303dffb52a0) Thanks [@thisnick](https://github.com/thisnick)! - Remove unreliable shell-out for heartbeat wake; auth notifications use passive system events instead.

## 0.4.0

### Minor Changes

- [#27](https://github.com/thisnick/agent-wechat/pull/27) [`8b07604`](https://github.com/thisnick/agent-wechat/commit/8b076041933b892d3361398646ddf1deb2268fc5) Thanks [@thisnick](https://github.com/thisnick)! - Proactive auth notifications: agent is notified immediately when WeChat auth is lost and can attempt re-login using cached credentials. Aligned all types with latest openclaw plugin SDK.

## 0.3.1

### Patch Changes

- [`0b45fba`](https://github.com/thisnick/agent-wechat/commit/0b45fba481778f5f7791b9787621270e1a9d1a23) Thanks [@thisnick](https://github.com/thisnick)! - Fix group message mention gating not working

  The monitor was not checking `msg.isMentioned` before dispatching group messages, so all group messages were processed regardless of `requireMention` config. Now:

  - Skips group messages that require mention but weren't mentioned
  - Sets `WasMentioned` in the inbound context for framework-level mention awareness

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
