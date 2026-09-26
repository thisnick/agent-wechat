# Agent guide for agent-wechat

This repository provides a programmable interface to a WeChat Linux client running in a Docker container. The Rust server controls the client UI and reads its local data; host clients use its authenticated HTTP API. Check the implementation before describing a behavior as supported. In particular, a registered route is not proof that it delivers useful events.

## Repository map

| Path | Responsibility |
| --- | --- |
| `packages/agent-server-rust/` | Axum server, UI plans, sessions, local data readers, media jobs |
| `packages/shared/` | TypeScript client and types generated from Rust |
| `packages/cli/` | `wx` command-line client and container management |
| `packages/openclaw-extension/` | OpenClaw channel, inbound polling and media, outbound messages |
| `packages/wechaty-puppet/` | Wechaty adapter |
| `packages/wechaty-gateway/` | Wechaty gateway |
| `docker/` | Image, entrypoint, and container-side tools |
| `docs/` | Public documentation site |

The active HTTP routes are assembled in `packages/agent-server-rust/src/router/mod.rs`. Request/response types live in `packages/agent-server-rust/src/ia/types.rs` and `packages/shared/src/`; keep them in sync with `pnpm generate-types` when changing Rust-exported types. The README is a concise user guide, not an exhaustive API specification.

## Working rules

- Use neutral, accurate technical language in code, comments, docs, commits, and PRs. Do not imply unauthorized access or conceal how a feature works.
- Keep account credentials, tokens, private infrastructure details, and research-only material out of this public repository. Do not link to private research repositories from public docs.
- Use an isolated test container and test account for behavior changes. Do not send messages to a live bot or modify a production container unless the task explicitly calls for it. Confirm the intended destination before any send.
- Preserve container data and backups unless removal is explicitly requested. The VNC viewer is read-only by default (`docker/entrypoint.sh` uses `x11vnc -viewonly`); do not change that for a shipped image. Temporary interactive debugging must not change the default.
- WeChat UI behavior is build- and architecture-sensitive. Avoid claiming a local unit test proves end-to-end behavior on another architecture. Verify actual chat selection and message creation in the client when changing UI control.
- Add a changeset for user-facing behavior or package changes. Documentation-only edits do not need one.

## Runtime boundaries

### UI plans and concurrency

The deterministic observe/identify/reduce/action loop is in `packages/agent-server-rust/src/execution/mod.rs`; UI states and plans are in `src/ia/states/` and `src/plans/`. UI-changing plans share `acquire_gui_guard()`, which serializes GUI work and pauses the health monitor for the duration. New UI operations must coordinate through that guard rather than run concurrently with login, chat selection, sending, or voice recording. Accessibility trees and screenshots are observations; recheck controls and the destination before committing a send.

Chat selection is implemented by `src/plans/chat_open.rs` and `src/tools/chat_select.rs`. A successful helper return is not enough by itself for a regression test: verify the selected chat in the UI and ensure the composer responds. Text, image, and file sends run through `src/plans/send_message.rs`.

### Local data and media

Chat, message, and media readers are under `packages/agent-server-rust/src/tools/wechat_*.rs`. Session metadata and access credentials are stored in the agent database (`/data/agent.db` by default); schema changes belong in a new `packages/agent-server-rust/migrations/V*__*.sql` migration. Do not assume a message or its media is present immediately after notification.

`GET /api/messages/{chat_id}/media/{local_id}` first checks locally available media. When native transfer metadata is available, the server can queue a transfer through `src/tools/media_download.rs`, wait up to 20 seconds for local bytes, and otherwise return `pending`. The transfer is server-owned; an HTTP disconnect does not cancel its submission. Callers should retry a pending result with a bounded policy. Never present raw message XML as a successfully retrieved attachment.

For image media, `quality=best` requests a native transfer only when the message advertises an Original. It returns the best cached variant and labels it in the `quality` field. The media endpoint never selects a chat: on the tested AMD64 build, the native full-image request does not fetch the standard-size variant. Opening a chat separately may cause WeChat to cache standard images in the visible history.

The current upload limit is defined in `src/router/mod.rs` (`MAX_UPLOAD_BYTES`, 128 MiB); HTTP body size includes base64 expansion. If changing limits, update the server, clients, error reporting, and boundary tests together.

### Outgoing voice notes

The voice endpoints are implemented in `src/router/voice.rs`: create a job with multipart audio and an `Idempotency-Key`, then inspect or cancel it by job ID. The container worker is `docker/tools/voice-send-worker`; audio setup is in `docker/tools/voice-audio-ready`. The worker prepares audio before opening the recorder, splits source audio at 50 seconds, feeds a dedicated virtual microphone, and verifies each outgoing note before starting the next. The 55-second recording deadline cancels rather than approaching the client's automatic cutoff.

Voice sends can partially succeed. Preserve the job's per-chunk statuses and message IDs; after an uncertain send, stop and report `needs_review` instead of resending. A reused idempotency key must not create a duplicate send. Keep the UI guard and health-monitor coordination intact. The CLI surface is `wx messages send <chatId> --voice <path> [--detach]` and `wx messages voice status|cancel <jobId>`.

### OpenClaw integration

The extension polls inbound messages in `packages/openclaw-extension/src/monitor.ts`, stores attachments through `src/attachment-store.ts`, and applies media/catch-up rules in `src/media-delivery.ts`. Group mention policy is separate from direct-message policy. Voice transcription, when configured, uses OpenClaw's media-understanding runtime after message gating. Outbound media is handled in `src/outbound-media.ts`; `audioAsVoice` requests a voice job, while ordinary audio remains a file attachment. Preserve pending/unavailable status and file paths rather than claiming media reached the model when it did not.

## Development and verification

```bash
pnpm install
pnpm build
pnpm typecheck
pnpm --filter @agent-wechat/agent-wechat test
pnpm --filter @agent-wechat/wechaty-puppet test
cargo test --manifest-path packages/agent-server-rust/Cargo.toml
```

Use `pnpm generate-types` after changing exported Rust types. Build images with `pnpm build:image:amd64` or `pnpm build:image:arm64`; `pnpm dev:deploy` updates a running development container, so identify the target before using it. For end-to-end changes, test the built image and CLI against an isolated container in addition to unit tests. The public CLI entry point is `wx`; `pnpm cli` runs the local build.

For user-facing changes, add a `.changeset/*.md` file with the affected package and appropriate patch/minor/major bump. Keep package docs and README commands aligned with the shipped CLI. Do not document stubs, unverified behavior, or planned features as working.
