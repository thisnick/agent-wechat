# @agent-wechat/wechat

## 0.11.15

### Patch Changes

- [#150](https://github.com/thisnick/agent-wechat/pull/150) [`80621fe`](https://github.com/thisnick/agent-wechat/commit/80621fec95a51b815785f92fa28403092caa0abd) Thanks [@thisnick](https://github.com/thisnick)! - fix(ci): pin npm to 11.12.1 for OIDC trusted publishing in release workflow

## 0.11.14

### Patch Changes

- [#148](https://github.com/thisnick/agent-wechat/pull/148) [`52056d9`](https://github.com/thisnick/agent-wechat/commit/52056d9b91243adcd890bf351af4a4a0e6c27604) Thanks [@thisnick](https://github.com/thisnick)! - Remove dead onboarding adapter that was deprecated in v0.11.10 but never deleted

## 0.11.13

## 0.11.12

### Patch Changes

- [#141](https://github.com/thisnick/agent-wechat/pull/141) [`5bd2938`](https://github.com/thisnick/agent-wechat/commit/5bd2938bda04def5d38b9e32146c3c5b26d45baf) Thanks [@thisnick](https://github.com/thisnick)! - Return "pending" instead of "unsupported" when voice data is not yet available in the database, so the extension retries instead of giving up.

## 0.11.11

### Patch Changes

- [#139](https://github.com/thisnick/agent-wechat/pull/139) [`c10d6ab`](https://github.com/thisnick/agent-wechat/commit/c10d6abc1dd96b1c4c6ef1b65968a62d6e0ace08) Thanks [@thisnick](https://github.com/thisnick)! - Add build profiles for new WeChat builds (3eda8254 aarch64, eba86b80 x86_64) with updated chat selection offsets and image XOR masks. Detach Frida hook after selectSession returns to restore function prologue.

## 0.11.10

### Patch Changes

- [#137](https://github.com/thisnick/agent-wechat/pull/137) [`9cb14d9`](https://github.com/thisnick/agent-wechat/commit/9cb14d9d0c153045805011b480b1fad780b9865c) Thanks [@thisnick](https://github.com/thisnick)! - Update openclaw dependency and fix breaking changes from plugin SDK refactor. Imports moved to dedicated subpaths (`channel-targets`, `config-runtime`, `command-auth`, `account-id`, `channel-reply-pipeline`). Removed deprecated `onboarding` adapter.

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

### Patch Changes

- [#84](https://github.com/thisnick/agent-wechat/pull/84) [`f76123f`](https://github.com/thisnick/agent-wechat/commit/f76123f2dd49d9fe6e64bbed8107f3a476480dd5) Thanks [@thisnick](https://github.com/thisnick)! - Stop retrying media poll when server returns unsupported type, and add video (type 43) to media types

## 0.9.3

### Patch Changes

- [#82](https://github.com/thisnick/agent-wechat/pull/82) [`06df0ad`](https://github.com/thisnick/agent-wechat/commit/06df0ad6a1b93989f09a26bee438695fd15e2232) Thanks [@thisnick](https://github.com/thisnick)! - Remove WAL checkpoint background task

## 0.9.2

### Patch Changes

- [#80](https://github.com/thisnick/agent-wechat/pull/80) [`ed43536`](https://github.com/thisnick/agent-wechat/commit/ed43536f1feaa28e0c276627dc4fdfda22870e75) Thanks [@thisnick](https://github.com/thisnick)! - Fix window activation targeting wrong window (e.g. "What's New" popup instead of main WeChat window)

## 0.9.1

### Patch Changes

- [#78](https://github.com/thisnick/agent-wechat/pull/78) [`5540a7f`](https://github.com/thisnick/agent-wechat/commit/5540a7f11283b5491d4f07e931fda13b7120be69) Thanks [@thisnick](https://github.com/thisnick)! - Update openclaw dependency to >=2026.3.2 and adapt readAllowFromStore call to new object-parameter signature

## 0.9.0

## 0.8.5

### Patch Changes

- [#72](https://github.com/thisnick/agent-wechat/pull/72) [`6be485a`](https://github.com/thisnick/agent-wechat/commit/6be485a7d7554d7e72e9e789ac011708eaa8f289) Thanks [@thisnick](https://github.com/thisnick)! - Fix @agent /command regex to support multi-word agent display names by using WeChat's hair space (U+2005) as the mention boundary instead of splitting on all whitespace

## 0.8.4

## 0.8.3

## 0.8.2

### Patch Changes

- [#66](https://github.com/thisnick/agent-wechat/pull/66) [`d730a10`](https://github.com/thisnick/agent-wechat/commit/d730a100a22a92d68bfa629a7f2c632befe3265c) Thanks [@thisnick](https://github.com/thisnick)! - Restore reliable group command handling for mention-prefixed commands such as `@agent /compact`.

  - Normalize WeChat command bodies so leading group mention tokens are stripped before command detection/authorization.
  - Use command-aware detection (`isControlCommandMessage`) in monitor gating paths.
  - Add a WeChat mention adapter so downstream command parsing also sees normalized command text.
  - Add tests covering mention-prefixed command normalization behavior.

## 0.8.1

### Patch Changes

- [#65](https://github.com/thisnick/agent-wechat/pull/65) [`e695cb5`](https://github.com/thisnick/agent-wechat/commit/e695cb5a1de0e747bd85037c29eb77ec484bcf1a) Thanks [@thisnick](https://github.com/thisnick)! - Harden WeChat inbound policy and command handling to align with OpenClaw channel security patterns.

  - Add centralized access-control logic for DM/group policy resolution and inbound decisions.
  - Normalize WeChat IDs/allowlists (including wildcard support) before authorization checks.
  - Compute and pass `CommandAuthorized` in inbound context and block unauthorized group control commands.
  - Apply mention gating with authorized command bypass behavior and fix segment-level mention handling.
  - Disable NO_REPLY command-window batching by isolating command-bearing messages into per-message dispatch.
  - Add group override support (`enabled`, `groupPolicy`, `allowFrom`) and align onboarding/docs semantics for `groupAllowFrom`.
  - Add unit tests for policy resolution, authorization, and mention/command gating behavior.

## 0.8.0

## 0.7.10

### Patch Changes

- [#56](https://github.com/thisnick/agent-wechat/pull/56) [`59e6061`](https://github.com/thisnick/agent-wechat/commit/59e6061e6836b683e734e80b7f9df82aca40d050) Thanks [@thisnick](https://github.com/thisnick)! - Revert READ_ONLY + busy_timeout DB reads back to immutable=1 with WAL checkpoint task. The READ_ONLY approach from #53 did not work as expected.

## 0.7.9

### Patch Changes

- [#53](https://github.com/thisnick/agent-wechat/pull/53) [`68a9a4e`](https://github.com/thisnick/agent-wechat/commit/68a9a4ea3871a7a4ee951ed29841fb5431924949) Thanks [@thisnick](https://github.com/thisnick)! - Fix stale WeChat DB reads by replacing immutable=1 with READ_ONLY + busy_timeout. WeChat DBs likely use DELETE journal mode where immutable=1 skips change-detection entirely. Also adds journal_mode logging to confirm the actual mode.

## 0.7.8

### Patch Changes

- [#51](https://github.com/thisnick/agent-wechat/pull/51) [`2d035a7`](https://github.com/thisnick/agent-wechat/commit/2d035a78544cee2b949a64cadaeee32ab0314400) Thanks [@thisnick](https://github.com/thisnick)! - Add periodic WAL checkpoint for fresh WeChat DB reads. A background task runs PASSIVE checkpoint every 3s, flushing WAL to the main DB file so immutable=1 reads see up-to-date data.

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
